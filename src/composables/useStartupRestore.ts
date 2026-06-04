import { watch, type Ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { usePaneStore } from '../stores/pane'
import { useProjectStore } from '../stores/project'
import { useDevSettingsStore, clampScrollback } from '../stores/devSettings'
import { useFilesSettingsStore } from '../stores/filesSettings'
import { waitForShellIdle } from '../utils/shellIdle'
import type { ArbiterConfig } from '../types/config'
import type { PaneNode } from '../types/pane'

function collectLeafIds(node: PaneNode): string[] {
  if (node.type === 'terminal') return [node.id]
  return [...collectLeafIds(node.first), ...collectLeafIds(node.second)]
}

/** Create PTY sessions eagerly for inactive worktrees of a single project
 *  workspace so Claude can resume/launch in the background before the user
 *  switches to that tab. Active panes mount normally and create their own
 *  sessions. Exported so newly-created workspaces (with adopted linked
 *  worktrees) can bootstrap the same way restored workspaces do. */
export async function bootstrapWorkspaceSessions(workspaceId: string) {
  const store = usePaneStore()
  const projectStore = useProjectStore()
  const devStore = useDevSettingsStore()

  const ws = store.workspaces.find(w => w.id === workspaceId)
  if (!ws || ws.type !== 'project') return

  const isWindows = navigator.platform.startsWith('Win')
  const gitBashPath = isWindows ? await invoke<string | null>('check_git_bash') : null

  // Size background sessions to match the active worktree's terminal.
  // Starting at 80×24 would trigger a large SIGWINCH when the user switches
  // worktrees, which makes Claude's Ink TUI redraw with a ghost cursor.
  const activeWt = ws.worktrees.find(wt => wt.id === ws.activeWorktreeId)
  const refPaneId = activeWt?.claudePaneId || (activeWt ? collectLeafIds(activeWt.root)[0] : null)
  let refCols = 80, refRows = 24
  if (refPaneId) {
    const refSid = await new Promise<string>((resolve) => {
      const existing = store.getPtySession(refPaneId)
      if (existing) { resolve(existing); return }
      const unwatch = watch(() => store.getPtySession(refPaneId), (sid) => {
        if (sid) { unwatch(); resolve(sid) }
      })
    })
    await new Promise<void>(r => requestAnimationFrame(() => requestAnimationFrame(() => r())))
    const size = await invoke<[number, number] | null>('get_session_size', { sessionId: refSid })
    if (size) { refCols = size[0]; refRows = size[1] }
  }

  const paneIds = ws.worktrees
    .filter(wt => wt.id !== ws.activeWorktreeId)
    .flatMap(wt => collectLeafIds(wt.root))

  for (const paneId of paneIds) {
    if (store.getPtySession(paneId)) continue
    const cwd = store.consumeSavedCwd(paneId)
    const claudeRestore = store.consumeSavedClaudeRestore(paneId)
    const savedShell = store.consumeSavedShell(paneId)
    const shellType = savedShell ?? (devStore.defaultShell === 'gitbash' ? 'gitbash' : 'powershell')
    const shellPath = (shellType === 'gitbash' && gitBashPath) ? gitBashPath : null
    store.setTerminalShell(paneId, shellPath ? 'gitbash' : 'powershell')

    try {
      const sessionId = await invoke<string>('create_session', { cols: refCols, rows: refRows, cwd: cwd ?? null, shell: shellPath })
      store.setPtySession(paneId, sessionId)

      if (claudeRestore) {
        // Flip the worktree card from "Terminal" → "Idle" optimistically
        const wtId = projectStore.getWorktreeIdForPane(paneId)
        if (wtId) projectStore.updateClaudeStatus(wtId, { status: 'ready' })

        if (claudeRestore.sessionId) {
          store.updateClaudePaneState(paneId, {
            lifecycle: 'launching', confirmed: false, sessionId: claudeRestore.sessionId,
          })
          store.armClaudeListeners(paneId)
          // Wait for OSC 133 idle before typing — otherwise on a slow shell
          // startup we'd send "claude --resume" before the prompt is ready.
          waitForShellIdle(sessionId).then(() => {
            invoke('write_to_session', { sessionId, data: `claude --resume ${claudeRestore.sessionId}\r` })
              .catch(e => console.error(`Arbiter: claude --resume write failed for pane ${paneId}:`, e))
          })
        } else if (claudeRestore.wasOpen) {
          store.updateClaudePaneState(paneId, { lifecycle: 'launching', confirmed: false })
          store.armClaudeListeners(paneId)
          waitForShellIdle(sessionId).then(() => {
            invoke('write_to_session', { sessionId, data: 'claude\r' })
              .catch(e => console.error(`Arbiter: claude launch write failed for pane ${paneId}:`, e))
          })
        }
      }
    } catch (e) {
      // Log so a failed background bootstrap doesn't silently leave a worktree
      // with no PTY (clicking it would show an empty unresponsive terminal).
      console.error(`Arbiter: bootstrap session creation failed for pane ${paneId}:`, e)
    }
  }
}

async function bootstrapBackgroundSessions() {
  const store = usePaneStore()
  const ids = store.workspaces.filter(w => w.type === 'project').map(w => w.id)
  for (const id of ids) await bootstrapWorkspaceSessions(id)
}

export async function loadAndRestore(overviewOpen: Ref<boolean>) {
  const store = usePaneStore()
  try {
    const config = await invoke<ArbiterConfig | null>('load_config')
    if (!config) return

    // Window size/position are applied in Rust during setup (before show())
    // so the window appears at its saved geometry without a visible snap.

    if (config.workspaces?.length) {
      store.restoreAllWorkspaces(config.workspaces, config.activeWorkspaceIndex)
    } else if (config.layout && config.terminals) {
      store.restoreFromSaved(config.layout, config.terminals, config.focusedTerminalIndex)
    }

    if (config.filesSettings) {
      const fs = useFilesSettingsStore()
      fs.setScreenshotFolder(config.filesSettings.screenshotFolder ?? null)
      fs.setLastDocsFolder(config.filesSettings.lastDocsFolder ?? null)
    }

    if (config.devSettings) {
      const dev = useDevSettingsStore()
      if (config.devSettings.useCustomTerminalBg === false) dev.useCustomTerminalBg = false
      if (config.devSettings.hideClaudeButtons === true) dev.hideClaudeButtons = true
      if (config.devSettings.hideShellButton === true) dev.hideShellButton = true
      if (config.devSettings.overviewClaudeOnly === false) dev.overviewClaudeOnly = false
      if (typeof config.devSettings.scrollback === 'number') dev.scrollback = clampScrollback(config.devSettings.scrollback)
    }

    if (config.overviewVisible) {
      // Geometry is applied at window-creation time in Rust setup() (so the
      // webview gets the right DPI on multi-monitor setups); here we just show it.
      overviewOpen.value = true
      invoke('show_overview_window')
    }

    // Populate project store paneToWorktree map BEFORE bootstrap so the
    // per-pane listeners can resolve worktreeIds in event handlers. Otherwise
    // the first claude-started event can fire before init registers the pane
    // → worktree mapping, and the card status update is dropped on the floor.
    const projectStore = useProjectStore()
    projectStore.registerAllProjectPanes()

    // Reconcile each project workspace against the on-disk state BEFORE
    // spawning background PTYs — otherwise a worktree whose folder was
    // deleted while Arbiter was closed would spawn a shell that falls back
    // to $HOME and launch Claude from there.
    await Promise.all(
      store.workspaces
        .filter(ws => ws.type === 'project')
        .map(ws => projectStore.reconcileWorktrees(ws.id)),
    )

    bootstrapBackgroundSessions()
  } catch (e) {
    // Config load failed. The Rust side quarantines a corrupt config.json to
    // config.json.corrupt-<ts> before reporting the error, so the autosave
    // that's about to start won't overwrite recoverable data — but log the
    // cause loudly so the user (or a bug report) can find the quarantine.
    console.error('Arbiter: config load failed, starting with fresh state. Cause:', e)
  }
}
