import { watch, type Ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { usePaneStore } from '../stores/pane'
import { useFilesSettingsStore } from '../stores/filesSettings'
import { useDevSettingsStore } from '../stores/devSettings'
import type {
  ArbiterConfig, SavedTerminal, SavedWorkspace,
  SavedTerminalWorkspace, SavedProjectWorkspace,
} from '../types/config'

/** Build a SavedTerminal by merging live PTY state (preferred) with in-memory
 *  saved state (fallback for panes that haven't mounted yet). */
async function enrichTerminal(store: ReturnType<typeof usePaneStore>, t: { id: string; name: string }): Promise<SavedTerminal> {
  const entry: SavedTerminal = { name: t.name }
  const sessionId = store.getPtySession(t.id)
  if (sessionId) {
    try {
      const cwd = await invoke<string | null>('get_session_cwd', { sessionId })
      if (cwd) entry.cwd = cwd
    } catch { /* ignore */ }
    const claudeSave = store.getClaudeSessionForSave(t.id)
    if (claudeSave.sessionId) entry.claudeSessionId = claudeSave.sessionId
    if (claudeSave.wasOpen) entry.claudeWasRunning = true
    const shell = store.getTerminalShell(t.id)
    if (shell !== 'powershell') entry.shell = shell
  } else {
    // Without the saved-state fallback, freshly-created worktrees (whose panes
    // haven't mounted yet) and restored background workspaces (where bootstrap
    // hasn't yet attached PTYs) get persisted as empty terminals — wiping
    // cwd / claude-resume info from disk.
    const savedCwd = store.getSavedCwd(t.id)
    if (savedCwd) entry.cwd = savedCwd
    const savedRestore = store.savedClaudeRestore[t.id]
    if (savedRestore) {
      if (savedRestore.sessionId) entry.claudeSessionId = savedRestore.sessionId
      if (savedRestore.wasOpen) entry.claudeWasRunning = true
    } else {
      const claudeSave = store.getClaudeSessionForSave(t.id)
      if (claudeSave.sessionId) entry.claudeSessionId = claudeSave.sessionId
      if (claudeSave.wasOpen) entry.claudeWasRunning = true
    }
    const savedShell = store.getSavedShell(t.id)
    if (savedShell && savedShell !== 'powershell') entry.shell = savedShell
  }
  return entry
}

export function useAutosave(ready: Ref<boolean>, overviewOpen: Ref<boolean>) {
  const store = usePaneStore()
  const filesStore = useFilesSettingsStore()
  const devStore = useDevSettingsStore()

  let saveInFlight = false
  let savePending = false
  // Debounce coalesces rapid-fire reactive changes (e.g. each token update in
  // claudePaneStates) into a single disk write. The in-flight/pending pair
  // below still serializes overlapping saves; the timer avoids spamming them.
  const DEBOUNCE_MS = 500
  let debounceTimer: ReturnType<typeof setTimeout> | null = null

  function scheduleAutoSave() {
    if (debounceTimer) clearTimeout(debounceTimer)
    debounceTimer = setTimeout(() => {
      debounceTimer = null
      performAutoSave()
    }, DEBOUNCE_MS)
  }

  async function performAutoSave() {
    if (!ready.value) return
    if (saveInFlight) {
      savePending = true
      return
    }
    saveInFlight = true
    try {
      const config: ArbiterConfig = {}

      const win = getCurrentWindow()
      try {
        // We persist `maximized` and apply it from Rust before show() to avoid
        // the launch blink that the plugin-driven async restore caused. When
        // currently maximized (or in fullscreen, which the plugin handles),
        // keep the previously-saved windowed geometry so un-maximizing returns
        // to a sensible size.
        const [maximized, fullscreen] = await Promise.all([win.isMaximized(), win.isFullscreen()])
        if (maximized || fullscreen) {
          try {
            const existing = await invoke<Record<string, any> | null>('load_config')
            const base = (existing?.window as Record<string, any> | undefined) ?? {}
            config.window = { ...base, maximized } as ArbiterConfig['window']
          } catch { /* ignore */ }
        } else {
          const size = await win.innerSize()
          const pos = await win.outerPosition()
          if (size.width > 200 && size.height > 200 && pos.x > -10000 && pos.y > -10000 && pos.x < 10000 && pos.y < 10000) {
            config.window = { width: size.width, height: size.height, x: pos.x, y: pos.y, maximized: false }
          }
        }
      } catch { /* ignore */ }

      config.overviewVisible = overviewOpen.value
      try {
        const overviewState = await invoke<{ x: number; y: number; width: number; height: number } | null>('get_overview_state')
        if (overviewState) config.overview = overviewState
      } catch { /* ignore */ }

      const serialized = store.serializeAll()

      const savedWorkspaces: SavedWorkspace[] = []
      for (const ws of serialized.workspaces) {
        if (ws.type === 'project') {
          const savedWorktrees = []
          for (const wt of ws.worktrees) {
            const terminals = await Promise.all(wt.terminals.map(t => enrichTerminal(store, t)))
            savedWorktrees.push({
              branchName: wt.branchName,
              path: wt.path,
              isMain: wt.isMain,
              parentBranch: wt.parentBranch,
              claudePaneIndex: wt.claudePaneIndex,
              defaultTerminalIndex: wt.defaultTerminalIndex,
              layout: wt.layout,
              terminals,
              explorerExpandedPaths: wt.explorerExpandedPaths,
            })
          }
          savedWorkspaces.push({
            type: 'project' as const,
            name: ws.name,
            repoRoot: ws.repoRoot,
            worktrees: savedWorktrees,
            activeWorktreeId: ws.activeWorktreeId,
          } satisfies SavedProjectWorkspace)
        } else {
          const terminals = await Promise.all(ws.terminals.map(t => enrichTerminal(store, t)))
          savedWorkspaces.push({
            type: 'terminal' as const,
            name: ws.name,
            layout: ws.layout,
            terminals,
            focusedTerminalIndex: ws.focusedTerminalIndex,
          } satisfies SavedTerminalWorkspace)
        }
      }

      config.workspaces = savedWorkspaces
      config.activeWorkspaceIndex = serialized.activeWorkspaceIndex

      if (filesStore.screenshotFolder || filesStore.lastDocsFolder) {
        config.filesSettings = {
          screenshotFolder: filesStore.screenshotFolder ?? undefined,
          lastDocsFolder: filesStore.lastDocsFolder ?? undefined,
        }
      }

      // Only persist when it diverges from the default (true) — keeps configs
      // from older builds untouched until the user actually toggles it.
      if (!devStore.useCustomTerminalBg) {
        config.devSettings = { useCustomTerminalBg: false }
      }

      await invoke('save_config', { config })
    } catch (e) {
      console.error('Auto-save failed:', e)
    } finally {
      saveInFlight = false
      if (savePending) {
        savePending = false
        // Run again to capture changes that arrived during the in-flight save
        performAutoSave()
      }
    }
  }

  // Every reactive state change runs this watcher, which schedules a
  // debounced save. saveInFlight/savePending still serialize overlapping
  // saves so we never race on the file if a save outruns the debounce.
  watch(
    () => [
      store.workspaces,
      store.activeWorkspaceIndex,
      store.terminalStatuses,
      filesStore.screenshotFolder,
      filesStore.lastDocsFolder,
      devStore.useCustomTerminalBg,
      // Narrow projection of claudePaneStates: only fields that actually get
      // persisted (lifecycle, sessionId, confirmed). Deep-watching the full
      // map would re-fire — and trigger O(N) get_session_cwd IPC roundtrips —
      // on every per-token Claude status update, which can be several per
      // second per pane during streaming.
      Object.entries(store.claudePaneStates)
        .map(([id, s]) => `${id}:${s.lifecycle}:${s.sessionId ?? ''}:${s.confirmed ? 1 : 0}`)
        .join('|'),
    ],
    scheduleAutoSave,
    { deep: true },
  )

  // Window geometry isn't in the Vue store, so the reactive watcher above
  // doesn't see resize/move. Without these listeners the user can move/resize
  // and quit (especially via Cmd+Q on macOS, which bypasses the JS close
  // handler) and nothing is saved.
  getCurrentWindow().onResized(scheduleAutoSave).catch(() => { /* best-effort */ })
  getCurrentWindow().onMoved(scheduleAutoSave).catch(() => { /* best-effort */ })

  /** Force a final save, bypassing the in-flight guard and debounce timer.
   *  Call on window close. */
  async function flush() {
    if (debounceTimer) { clearTimeout(debounceTimer); debounceTimer = null }
    saveInFlight = false
    savePending = false
    await performAutoSave()
  }

  return { flush }
}
