import { listen } from '@tauri-apps/api/event'
import { invoke } from '@tauri-apps/api/core'
import type { ClaudePaneState } from '../types/pane'
import { computeCost } from '../utils/claudePricing'

interface ClaudeStatusPayload {
  session_id: string
  model_id?: string | null
  input_tokens?: number | null
  output_tokens?: number | null
  cache_creation_input_tokens?: number | null
  cache_read_input_tokens?: number | null
  folder?: string | null
  branch?: string | null
}

export interface ClaudeEventDeps {
  getClaudePaneState: (paneId: string) => ClaudePaneState
  updateClaudePaneState: (paneId: string, update: Partial<ClaudePaneState>) => void
  setTerminalStatus: (paneId: string, status: 'idle' | 'running' | 'ready' | 'working' | 'attention') => void
}

export interface ClaudeEventTimers {
  idleTimers: Record<string, ReturnType<typeof setTimeout>>
  launchTimestamps: Record<string, number>
  turnBaselines: Record<string, number>
  resizeTimestamps: Record<string, number>
}

/** Wire the 7 Tauri event listeners that drive Claude lifecycle state for a
 *  pane. Returns unlisten functions — the caller is responsible for storing
 *  them per-pane and invoking them on teardown. */
export async function wireClaudeEventListeners(
  paneId: string,
  sid: string,
  deps: ClaudeEventDeps,
  timers: ClaudeEventTimers,
): Promise<Array<() => void>> {
  const { getClaudePaneState, updateClaudePaneState, setTerminalStatus } = deps
  const { idleTimers, launchTimestamps, turnBaselines, resizeTimestamps } = timers

  const listeners: Array<() => void> = []

  // When no activity arrives within 300ms, revert from working → ready.
  function resetIdleTimer() {
    if (idleTimers[paneId]) clearTimeout(idleTimers[paneId])
    idleTimers[paneId] = setTimeout(() => {
      const s = getClaudePaneState(paneId)
      if (s.lifecycle === 'working') {
        turnBaselines[paneId] = s.outputTokens
        updateClaudePaneState(paneId, { lifecycle: 'ready' })
      }
      delete idleTimers[paneId]
    }, 300)
  }

  // claude-started: JSONL adopted → Claude confirmed running
  const unStarted = await listen<ClaudeStatusPayload>(`claude-started-${sid}`, (event) => {
    const p = event.payload
    const update: Partial<ClaudePaneState> = { lifecycle: 'ready', confirmed: true }
    if (p?.session_id) update.sessionId = p.session_id
    if (p?.model_id) update.model = p.model_id
    if (p?.input_tokens != null) update.inputTokens = p.input_tokens
    if (p?.output_tokens != null) update.outputTokens = p.output_tokens
    if (p?.cache_creation_input_tokens != null) update.cacheWriteTokens = p.cache_creation_input_tokens
    if (p?.cache_read_input_tokens != null) update.cacheReadTokens = p.cache_read_input_tokens
    delete launchTimestamps[paneId]
    // -1 = sentinel to capture from next claude-status
    turnBaselines[paneId] = p?.output_tokens ?? -1
    updateClaudePaneState(paneId, update)
  })
  listeners.push(unStarted as unknown as () => void)

  // claude-activity: spinner detection from backend PTY reader. Suppressed
  // for 500ms after resize (redraws contain Braille chars from Claude's status bar).
  const unActivity = await listen<string>(`claude-activity-${sid}`, (event) => {
    const activity = event.payload as 'thinking' | 'generating'
    const state = getClaudePaneState(paneId)

    if (activity === 'thinking') {
      const lastResize = resizeTimestamps[paneId] ?? 0
      if (lastResize > 0 && Date.now() - lastResize < 500) return

      if (state.lifecycle === 'ready' || state.lifecycle === 'launching') {
        updateClaudePaneState(paneId, { lifecycle: 'working' })
      }
    }
    if (state.lifecycle === 'working' || getClaudePaneState(paneId).lifecycle === 'working') {
      resetIdleTimer()
    }
  })
  listeners.push(unActivity as unknown as () => void)

  // claude-status: token/model/cost updates from JSONL. Also a backup gate
  // for ready → working via output_tokens increase (when spinner detection
  // is suppressed by a recent resize).
  const unStatus = await listen<ClaudeStatusPayload>(`claude-status-${sid}`, (event) => {
    const p = event.payload
    const state = getClaudePaneState(paneId)
    const update: Partial<ClaudePaneState> = {}

    if (p?.output_tokens != null && turnBaselines[paneId] === -1) {
      turnBaselines[paneId] = p.output_tokens
    }
    if (p?.output_tokens != null && p.output_tokens > (turnBaselines[paneId] ?? 0)) {
      if (state.lifecycle === 'ready' || state.lifecycle === 'launching') {
        update.lifecycle = 'working'
      }
      resetIdleTimer()
    }

    if (p?.session_id) update.sessionId = p.session_id
    if (p?.model_id) update.model = p.model_id
    if (p?.input_tokens != null) update.inputTokens = p.input_tokens
    if (p?.output_tokens != null) update.outputTokens = p.output_tokens
    if (p?.cache_creation_input_tokens != null) update.cacheWriteTokens = p.cache_creation_input_tokens
    if (p?.cache_read_input_tokens != null) update.cacheReadTokens = p.cache_read_input_tokens
    if (p) {
      const total = (p.input_tokens ?? 0) + (p.output_tokens ?? 0)
        + (p.cache_creation_input_tokens ?? 0) + (p.cache_read_input_tokens ?? 0)
      update.contextPercent = Math.min(100, (total / 200_000) * 100)
      const model = update.model ?? state.model
      update.cost = computeCost(
        model,
        p.input_tokens ?? state.inputTokens,
        p.output_tokens ?? state.outputTokens,
        p.cache_read_input_tokens ?? state.cacheReadTokens,
        p.cache_creation_input_tokens ?? state.cacheWriteTokens,
      )
    }
    if (state.lifecycle === 'working') {
      resetIdleTimer()
    }
    updateClaudePaneState(paneId, update)
  })
  listeners.push(unStatus as unknown as () => void)

  // claude-exited: process died → closed
  const unExited = await listen(`claude-exited-${sid}`, () => {
    if (idleTimers[paneId]) { clearTimeout(idleTimers[paneId]); delete idleTimers[paneId] }
    delete launchTimestamps[paneId]
    delete turnBaselines[paneId]
    updateClaudePaneState(paneId, { lifecycle: 'closed', confirmed: false })
  })
  listeners.push(unExited as unknown as () => void)

  // shell-activity: exit fallback via OSC 133 idle detection
  const unShellActivity = await listen<boolean>(`shell-activity-${sid}`, (event) => {
    const idle = event.payload
    const state = getClaudePaneState(paneId)

    if (idle && (state.lifecycle === 'ready' || state.lifecycle === 'working' || state.lifecycle === 'attention')) {
      if (idleTimers[paneId]) { clearTimeout(idleTimers[paneId]); delete idleTimers[paneId] }
      delete launchTimestamps[paneId]
      updateClaudePaneState(paneId, { lifecycle: 'closed', confirmed: false })
      invoke('clear_claude_monitor', { sessionId: sid }).catch(() => {})
    } else if (idle && state.lifecycle === 'launching') {
      const launchedAt = launchTimestamps[paneId] ?? 0
      if (launchedAt > 0 && Date.now() - launchedAt > 5000) {
        delete launchTimestamps[paneId]
        updateClaudePaneState(paneId, { lifecycle: 'closed', confirmed: false })
        invoke('clear_claude_monitor', { sessionId: sid }).catch(() => {})
      }
    }

    if (state.lifecycle === 'closed') {
      setTerminalStatus(paneId, idle ? 'idle' : 'running')
    }
  })
  listeners.push(unShellActivity as unknown as () => void)

  // claude-bell: BEL → end of turn (working → ready) or attention (ready → attention)
  const unBell = await listen(`claude-bell-${sid}`, () => {
    const state = getClaudePaneState(paneId)
    if (idleTimers[paneId]) { clearTimeout(idleTimers[paneId]); delete idleTimers[paneId] }
    if (state.lifecycle === 'working') {
      turnBaselines[paneId] = state.outputTokens
      updateClaudePaneState(paneId, { lifecycle: 'ready' })
    } else if (state.lifecycle === 'ready') {
      updateClaudePaneState(paneId, { lifecycle: 'attention' })
    }
  })
  listeners.push(unBell as unknown as () => void)

  // claude-context: backend-parsed context %
  const unContext = await listen<number>(`claude-context-${sid}`, (event) => {
    updateClaudePaneState(paneId, { contextPercent: event.payload })
  })
  listeners.push(unContext as unknown as () => void)

  return listeners
}
