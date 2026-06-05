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
  // Claude is blocked on the user via an interaction tool (AskUserQuestion /
  // ExitPlanMode). Reliable from the transcript.
  awaiting_input?: boolean | null
  // A tool_use is pending (running, or awaiting a permission prompt). Combined
  // with a BEL this distinguishes a permission wait (attention) from a finished
  // turn (ready).
  pending_tool_use?: boolean | null
  folder?: string | null
  branch?: string | null
}

// Authoritative context usage from Claude's statusLine capture (Tier 2).
interface ClaudeContextPayload {
  session_id: string
  model_id?: string | null
  context_window_size?: number | null
  used_percentage?: number | null
  input_tokens?: number | null
  output_tokens?: number | null
  cache_creation_input_tokens?: number | null
  cache_read_input_tokens?: number | null
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

  // Whether a tool_use is pending for this pane (latest from claude-status).
  // Used by the BEL handler to tell a permission wait from a finished turn.
  let pendingToolUse = false
  // When the Stop hook last fired. The final assistant message hits the JSONL
  // (and Claude redraws its prompt) right around turn-end, which would re-flip
  // the pane to 'working' just after we set 'ready'. Suppress working for a
  // short window after Stop so the animation halts instantly and stays halted.
  let lastStopAt = 0
  const STOP_SUPPRESS_MS = 700

  // 'working' is sticky within a turn: Claude pauses between tool calls, so
  // reverting on a short gap would flicker the status (and the header gradient)
  // mid-turn. A definitive end-of-turn (BEL with no pending tool, or shell idle)
  // reverts immediately; this fallback only fires after a longer lull.
  function resetIdleTimer() {
    if (idleTimers[paneId]) clearTimeout(idleTimers[paneId])
    idleTimers[paneId] = setTimeout(() => {
      const s = getClaudePaneState(paneId)
      if (s.lifecycle === 'working') {
        turnBaselines[paneId] = s.outputTokens
        updateClaudePaneState(paneId, { lifecycle: 'ready' })
      }
      delete idleTimers[paneId]
    }, 2000)
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

      // ready/launching → working, and attention → working (Claude resumed
      // after the user answered a prompt/question). Suppressed briefly after a
      // Stop so the end-of-turn redraw doesn't revive the animation.
      if (Date.now() - lastStopAt >= STOP_SUPPRESS_MS
        && (state.lifecycle === 'ready' || state.lifecycle === 'launching' || state.lifecycle === 'attention')) {
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

    if (p?.pending_tool_use != null) pendingToolUse = p.pending_tool_use

    if (p?.output_tokens != null && turnBaselines[paneId] === -1) {
      turnBaselines[paneId] = p.output_tokens
    }
    if (p?.output_tokens != null && p.output_tokens > (turnBaselines[paneId] ?? 0)) {
      // Output grew → Claude is generating; resume from ready/launching and also
      // from attention (the user answered and Claude is responding again).
      // Suppressed briefly after a Stop: the turn's FINAL message lands here and
      // would otherwise re-flip 'working' right after Stop set 'ready'.
      if (Date.now() - lastStopAt >= STOP_SUPPRESS_MS
        && (state.lifecycle === 'ready' || state.lifecycle === 'launching' || state.lifecycle === 'attention')) {
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
      // contextPercent is owned by the Tier-2 statusLine capture (claude-context);
      // don't derive an approximate /200k value here that would fight it.
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
    // Transcript says Claude is blocked on a question / plan approval — this is
    // authoritative, so it overrides any working transition computed above.
    if (p?.awaiting_input) {
      update.lifecycle = 'attention'
      if (idleTimers[paneId]) { clearTimeout(idleTimers[paneId]); delete idleTimers[paneId] }
    }
    updateClaudePaneState(paneId, update)
  })
  listeners.push(unStatus as unknown as () => void)

  // claude-exited: process died → closed. The shell is back at an idle prompt,
  // so reset the shell status too (getPaneStatus now reads it) — otherwise a
  // 'running' left over from launching `claude` shows as a stale pulse.
  const unExited = await listen(`claude-exited-${sid}`, () => {
    if (idleTimers[paneId]) { clearTimeout(idleTimers[paneId]); delete idleTimers[paneId] }
    delete launchTimestamps[paneId]
    delete turnBaselines[paneId]
    updateClaudePaneState(paneId, { lifecycle: 'closed', confirmed: false, hasContext: false })
    setTerminalStatus(paneId, 'idle')
  })
  listeners.push(unExited as unknown as () => void)

  // shell-activity: exit fallback via OSC 133 idle detection. The OSC-133 idle
  // can fire spuriously while Claude is still alive, which previously flipped a
  // running pane to 'closed' permanently (nothing re-opens it) and lost the
  // running state across restarts. So before closing, confirm with the backend
  // that no Claude process is actually alive. The authoritative PID-based
  // `claude-exited` remains the fast path; this is only the safety net.
  async function confirmExitThenClose(extra: Partial<ClaudePaneState>) {
    let running = true
    try {
      const info = await invoke<{ running: boolean }>('claude_persist_info', { sessionId: sid })
      running = info.running
    } catch {
      running = false // backend unreachable: trust the idle signal and close
    }
    if (running) return
    const s = getClaudePaneState(paneId)
    if (s.lifecycle === 'closed') return
    if (idleTimers[paneId]) { clearTimeout(idleTimers[paneId]); delete idleTimers[paneId] }
    delete launchTimestamps[paneId]
    updateClaudePaneState(paneId, { lifecycle: 'closed', confirmed: false, ...extra })
    invoke('clear_claude_monitor', { sessionId: sid })
      .catch(e => console.error(`clear_claude_monitor failed for ${sid}:`, e))
  }

  const unShellActivity = await listen<boolean>(`shell-activity-${sid}`, (event) => {
    const idle = event.payload
    const state = getClaudePaneState(paneId)

    if (idle && (state.lifecycle === 'ready' || state.lifecycle === 'working' || state.lifecycle === 'attention')) {
      confirmExitThenClose({})
    } else if (idle && state.lifecycle === 'launching') {
      const launchedAt = launchTimestamps[paneId] ?? 0
      if (launchedAt > 0 && Date.now() - launchedAt > 5000) {
        confirmExitThenClose({ hasContext: false })
      }
    }

    // Always track the shell's idle/busy. getPaneStatus only *uses* this when
    // Claude is closed, but it must be current at that moment: a 'running' set
    // while typing `claude` would otherwise persist as a stale green pulse after
    // Claude exits, since no further shell-activity edge fires at an idle prompt.
    setTerminalStatus(paneId, idle ? 'idle' : 'running')
  })
  listeners.push(unShellActivity as unknown as () => void)

  // claude-bell: Claude rings the BEL when it wants the user. Disambiguate via
  // the transcript's pending-tool flag:
  //   pending tool  → it's blocked on a permission prompt / menu  → attention
  //   no pending    → end-of-turn notification                    → ready
  // (AskUserQuestion / ExitPlanMode also surface as 'attention' directly from
  // claude-status's awaiting_input, independent of the bell.)
  const unBell = await listen(`claude-bell-${sid}`, () => {
    const state = getClaudePaneState(paneId)
    if (idleTimers[paneId]) { clearTimeout(idleTimers[paneId]); delete idleTimers[paneId] }
    if (pendingToolUse) {
      updateClaudePaneState(paneId, { lifecycle: 'attention' })
    } else if (state.lifecycle === 'working') {
      turnBaselines[paneId] = state.outputTokens
      updateClaudePaneState(paneId, { lifecycle: 'ready' })
    }
  })
  listeners.push(unBell as unknown as () => void)

  // claude-attention: the backend detected an interactive prompt (permission,
  // plan approval, AskUserQuestion) in the terminal output. These prompts are
  // not in the JSONL transcript while live, so this PTY-text signal is the
  // reliable trigger. Cleared automatically when Claude resumes (the activity /
  // output-token handlers move attention → working).
  const unAttention = await listen(`claude-attention-${sid}`, () => {
    const state = getClaudePaneState(paneId)
    if (state.lifecycle === 'ready' || state.lifecycle === 'working' || state.lifecycle === 'launching') {
      if (idleTimers[paneId]) { clearTimeout(idleTimers[paneId]); delete idleTimers[paneId] }
      updateClaudePaneState(paneId, { lifecycle: 'attention' })
    }
  })
  listeners.push(unAttention as unknown as () => void)

  // claude-stop: the Stop hook fired — Claude finished its turn. End the turn
  // promptly (working → ready) instead of waiting on the idle-timer fallback.
  // Leaves an active attention prompt untouched (Stop doesn't fire at a prompt).
  const unStop = await listen(`claude-stop-${sid}`, () => {
    lastStopAt = Date.now()
    const state = getClaudePaneState(paneId)
    if (state.lifecycle === 'working') {
      if (idleTimers[paneId]) { clearTimeout(idleTimers[paneId]); delete idleTimers[paneId] }
      turnBaselines[paneId] = state.outputTokens
      updateClaudePaneState(paneId, { lifecycle: 'ready' })
    }
  })
  listeners.push(unStop as unknown as () => void)

  // claude-context: authoritative context usage captured from Claude's own
  // statusLine payload (model + window size + used % + per-component tokens).
  // This is the source of truth for the footer — the JSONL transcript lacks
  // the window size and used %.
  const unContext = await listen<ClaudeContextPayload>(`claude-context-${sid}`, (event) => {
    const p = event.payload
    const update: Partial<ClaudePaneState> = { hasContext: true }
    if (p.model_id) update.model = p.model_id
    if (p.context_window_size != null) update.contextWindowSize = p.context_window_size
    if (p.used_percentage != null) {
      update.usedPercentage = p.used_percentage
      update.contextPercent = Math.min(100, p.used_percentage)
    }
    if (p.input_tokens != null) update.inputTokens = p.input_tokens
    if (p.output_tokens != null) update.outputTokens = p.output_tokens
    if (p.cache_creation_input_tokens != null) update.cacheWriteTokens = p.cache_creation_input_tokens
    if (p.cache_read_input_tokens != null) update.cacheReadTokens = p.cache_read_input_tokens
    const state = getClaudePaneState(paneId)
    update.cost = computeCost(
      update.model ?? state.model,
      update.inputTokens ?? state.inputTokens,
      update.outputTokens ?? state.outputTokens,
      update.cacheReadTokens ?? state.cacheReadTokens,
      update.cacheWriteTokens ?? state.cacheWriteTokens,
    )
    updateClaudePaneState(paneId, update)
  })
  listeners.push(unContext as unknown as () => void)

  return listeners
}
