import { watch, type Ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { usePaneStore } from '../stores/pane'
import { useProjectStore } from '../stores/project'
import { useDevSettingsStore } from '../stores/devSettings'
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
          setTimeout(() => {
            invoke('write_to_session', { sessionId, data: `claude --resume ${claudeRestore.sessionId}\r` })
          }, 500)
        } else if (claudeRestore.wasOpen) {
          store.updateClaudePaneState(paneId, { lifecycle: 'launching', confirmed: false })
          store.armClaudeListeners(paneId)
          setTimeout(() => {
            invoke('write_to_session', { sessionId, data: 'claude\r' })
          }, 500)
        }
      }
    } catch { /* ignore failed session creation */ }
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

    if (config.window) {
      const { width, height, x, y } = config.window
      // Reject bogus geometry: too small, or wildly off-screen
      if (width > 200 && height > 200 && x > -10000 && y > -10000 && x < 10000 && y < 10000) {
        const win = getCurrentWindow()
        try {
          await win.setSize(new (await import('@tauri-apps/api/dpi')).PhysicalSize(width, height))
          await win.setPosition(new (await import('@tauri-apps/api/dpi')).PhysicalPosition(x, y))
          await new Promise(r => setTimeout(r, 150))
        } catch { /* ignore if position is off-screen */ }
      }
    }

    if (config.workspaces?.length) {
      store.restoreAllWorkspaces(config.workspaces, config.activeWorkspaceIndex)
    } else if (config.layout && config.terminals) {
      store.restoreFromSaved(config.layout, config.terminals, config.focusedTerminalIndex)
    }

    if (config.overviewVisible && config.overview) {
      overviewOpen.value = true
      invoke('restore_overview_window', {
        x: config.overview.x, y: config.overview.y,
        width: config.overview.width, height: config.overview.height,
      })
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
  } catch {
    // Config load failed — start fresh
  }
}
