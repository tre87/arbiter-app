<script setup lang="ts">
import { ref, watch, onMounted, onBeforeUnmount } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import { usePaneStore } from './stores/pane'
import { useProjectStore } from './stores/project'
import { useDevSettingsStore } from './stores/devSettings'
import SplitView from './components/SplitView.vue'
import ProjectWorkspaceView from './components/ProjectWorkspaceView.vue'
import StatsBar from './components/StatsBar.vue'
import ConfirmDialog from './components/ConfirmDialog.vue'
import { useConfirm } from './composables/useConfirm'
import MdiIcon from './components/MdiIcon.vue'
import { mdiCogOutline, mdiKeyboardOutline, mdiViewDashboardOutline } from '@mdi/js'
import ShortcutsDialog from './components/ShortcutsDialog.vue'
import SettingsDialog from './components/SettingsDialog.vue'
import WorkspaceTabs from './components/WorkspaceTabs.vue'
import WindowControls from './components/WindowControls.vue'
import logoUrl from './assets/logo.svg'
import { computeLeafRects, findNeighbor, findResizableSplit, type Direction } from './utils/spatial'
import type { ArbiterConfig, SavedTerminal, SavedWorkspace, SavedTerminalWorkspace, SavedProjectWorkspace } from './types/config'
import type { PaneNode, Workspace } from './types/pane'

const store = usePaneStore()
const devStore = useDevSettingsStore()
const ready = ref(false)
const overviewOpen = ref(false)

// ── Auto-save (crash-safe persistence) ──────────────────────────────────────
// Purely event-driven: every reactive state change runs the watcher below,
// which calls performAutoSave once per Vue tick (mutations are batched). A
// simple in-flight/pending pair serializes overlapping saves so we never have
// two writers racing on the same file.

let saveInFlight = false
let savePending = false

async function performAutoSave() {
  if (!ready.value) return
  if (saveInFlight) {
    savePending = true
    return
  }
  saveInFlight = true
  try {
    const config: ArbiterConfig = {}

    // Save window geometry — only if values look sane
    const win = getCurrentWindow()
    try {
      const size = await win.innerSize()
      const pos = await win.outerPosition()
      if (size.width > 200 && size.height > 200 && pos.x > -10000 && pos.y > -10000 && pos.x < 10000 && pos.y < 10000) {
        config.window = { width: size.width, height: size.height, x: pos.x, y: pos.y }
      }
    } catch { /* ignore */ }

    // Save overview state (always persist visibility flag; geometry only if visible)
    config.overviewVisible = overviewOpen.value
    try {
      const overviewState = await invoke<{ x: number; y: number; width: number; height: number } | null>('get_overview_state')
      if (overviewState) config.overview = overviewState
    } catch { /* ignore */ }

    // Save all workspaces with current state
    const serialized = store.serializeAll()

    async function enrichTerminal(t: { id: string; name: string }): Promise<SavedTerminal> {
      const entry: SavedTerminal = { name: t.name }
      const sessionId = store.getPtySession(t.id)
      // Prefer live PTY state when the pane is mounted; otherwise fall back to
      // the saved* in-memory maps. Without this fallback, freshly-created
      // worktrees (whose panes haven't mounted yet) and restored background
      // workspaces (where bootstrap hasn't yet attached PTYs) get persisted as
      // empty terminals — wiping cwd / claude-resume info from disk.
      if (sessionId) {
        try {
          const cwd = await invoke<string | null>('get_session_cwd', { sessionId })
          if (cwd) entry.cwd = cwd
        } catch { /* ignore */ }
        const claudeId = store.getClaudeSessionId(t.id)
        if (claudeId) entry.claudeSessionId = claudeId
        if (store.isClaudeRunning(t.id)) entry.claudeWasRunning = true
        const shell = store.getTerminalShell(t.id)
        if (shell !== 'powershell') entry.shell = shell
      } else {
        const savedCwd = store.getSavedCwd(t.id)
        if (savedCwd) entry.cwd = savedCwd
        const savedClaudeId = store.getSavedClaudeSession(t.id)
        if (savedClaudeId) entry.claudeSessionId = savedClaudeId
        if (store.getSavedClaudeWasRunning(t.id) || store.isClaudeRunning(t.id)) entry.claudeWasRunning = true
        const savedShell = store.getSavedShell(t.id)
        if (savedShell && savedShell !== 'powershell') entry.shell = savedShell
      }
      return entry
    }

    const savedWorkspaces: SavedWorkspace[] = []
    for (const ws of serialized.workspaces) {
      if (ws.type === 'project') {
        const savedWorktrees = []
        for (const wt of ws.worktrees) {
          const terminals = await Promise.all(wt.terminals.map(enrichTerminal))
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
        const terminals = await Promise.all(ws.terminals.map(enrichTerminal))
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

// Watch store state and save on every change. Vue batches mutations within a
// tick, so a burst of updates produces one save call (or one queued one).
watch(
  () => [
    store.workspaces,
    store.activeWorkspaceIndex,
    store.terminalStatuses,
    store.claudeSessionIds,
  ],
  performAutoSave,
  { deep: true }
)

// ── Startup: load config and restore layout ──────────────────────────────────

async function loadAndRestore() {
  try {
    const config = await invoke<ArbiterConfig | null>('load_config')
    if (!config) return

    // Restore window geometry — validate before applying
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

    // Restore layout — prefer multi-workspace format, fall back to legacy
    if (config.workspaces?.length) {
      store.restoreAllWorkspaces(config.workspaces, config.activeWorkspaceIndex)
    } else if (config.layout && config.terminals) {
      store.restoreFromSaved(config.layout, config.terminals, config.focusedTerminalIndex)
    }

    // Restore overview window only if it was visible last session
    if (config.overviewVisible && config.overview) {
      overviewOpen.value = true
      invoke('restore_overview_window', {
        x: config.overview.x, y: config.overview.y,
        width: config.overview.width, height: config.overview.height,
      })
    }

    // Populate the project store's paneToWorktree map BEFORE bootstrap so
    // the listeners bootstrap attaches per pane can resolve worktreeIds from
    // the event handlers. (Otherwise the first claude-started event can fire
    // before init has registered the pane → worktree mapping, and the card
    // status update is dropped on the floor.) This is a synchronous helper;
    // the slower model fetches + refs watchers run via initAllProjectWorkspaces
    // after ready.value flips true.
    useProjectStore().registerAllProjectPanes()

    // Eagerly create PTY sessions for background workspace terminals.
    // The active workspace's terminals will be handled by TerminalPane onMounted.
    // Background terminals need sessions created now so Claude can start running
    // before the user switches to that tab.
    bootstrapBackgroundSessions()
  } catch {
    // Config load failed — start fresh
  }
}

function collectLeafIds(node: PaneNode): string[] {
  if (node.type === 'terminal') return [node.id]
  return [...collectLeafIds(node.first), ...collectLeafIds(node.second)]
}

function collectAllLeafIds(ws: Workspace): string[] {
  if (ws.type === 'project') {
    return ws.worktrees.flatMap(wt => collectLeafIds(wt.root))
  }
  return collectLeafIds(ws.root)
}

async function bootstrapBackgroundSessions() {
  // Detect Git Bash once for all background sessions
  const isWindows = navigator.platform.startsWith('Win')
  let gitBashPath: string | null = null
  if (isWindows) {
    gitBashPath = await invoke<string | null>('check_git_bash')
  }
  const projectStore = useProjectStore()

  for (let i = 0; i < store.workspaces.length; i++) {
    const ws = store.workspaces[i]
    const isActiveWs = i === store.activeWorkspaceIndex
    // For the active workspace: a terminal workspace's panes all mount
    // normally via TerminalPane, but a project workspace only mounts the
    // *active* worktree — the other worktrees need background bootstrap so
    // their Claude instances launch without the user clicking in.
    let paneIds: string[]
    if (isActiveWs) {
      if (ws.type !== 'project') continue
      paneIds = ws.worktrees
        .filter(wt => wt.id !== ws.activeWorktreeId)
        .flatMap(wt => collectLeafIds(wt.root))
    } else {
      paneIds = collectAllLeafIds(ws)
    }
    for (const paneId of paneIds) {
      if (store.getPtySession(paneId)) continue // already has a session
      const cwd = store.consumeSavedCwd(paneId)
      const claudeId = store.consumeSavedClaudeSession(paneId)
      const claudeWasRunning = store.consumeSavedClaudeWasRunning(paneId)

      // Use saved shell for this pane, fall back to default setting
      const savedShell = store.consumeSavedShell(paneId)
      const shellType = savedShell ?? (devStore.defaultShell === 'gitbash' ? 'gitbash' : 'powershell')
      const shellPath = (shellType === 'gitbash' && gitBashPath) ? gitBashPath : null
      store.setTerminalShell(paneId, shellPath ? 'gitbash' : 'powershell')

      try {
        const sessionId = await invoke<string>('create_session', { cols: 80, rows: 24, cwd: cwd ?? null, shell: shellPath })
        store.setPtySession(paneId, sessionId)
        // Directly attach project store listeners for worktree claude panes.
        // The project store's reactive watch on paneToWorktree → session would
        // also pick this up, but the store may not even be instantiated yet
        // on startup (initAllProjectWorkspaces runs after bootstrap), so the
        // watch isn't set up in time. Explicit call eliminates that race and
        // is a no-op for panes that aren't registered to a worktree.
        projectStore.ensurePaneListeners(paneId)

        if (claudeId || claudeWasRunning) {
          // Optimistically flip the worktree card from "Terminal" → "Idle"
          // the moment we decide to launch Claude. The claude-started event
          // listener will later fill in model/tokens when Claude reports in,
          // but this avoids the card sitting on "Terminal" for several
          // seconds (or forever if the JSONL detection path silently drops
          // the event for a background worktree).
          const wtId = projectStore.getWorktreeIdForPane(paneId)
          if (wtId) {
            projectStore.updateClaudeStatus(wtId, { status: 'ready' })
          }
        }

        if (claudeId) {
          // Register so it persists on save even if this tab is never visited
          store.setClaudeSessionId(paneId, claudeId, 1)
          setTimeout(() => {
            invoke('write_to_session', { sessionId, data: `claude --resume ${claudeId}\r` })
          }, 500)
        } else if (claudeWasRunning) {
          // Mark as running so isClaudeRunning returns true on save
          store.setClaudeSessionId(paneId, '', 0)
          setTimeout(() => {
            invoke('write_to_session', { sessionId, data: 'claude\r' })
          }, 500)
        }
      } catch { /* ignore failed session creation */ }
    }
  }
}

// ── Close intercept ──────────────────────────────────────────────────────────
// State is autosaved continuously, so the close handler just flushes any
// pending save and exits. No dialog, no save options.

async function setupCloseHandler() {
  const win = getCurrentWindow()
  await win.onCloseRequested(async (_event) => {
    // Autosave runs on every state change, so by the time the user clicks close
    // the disk is already up to date. No final flush needed.
    await invoke('exit_app')
  })
}

// ── Keyboard shortcuts ───────────────────────────────────────────────────────

const arrowToDirection: Record<string, Direction> = {
  ArrowLeft: 'left',
  ArrowRight: 'right',
  ArrowUp: 'up',
  ArrowDown: 'down',
}

function handleKeyDown(e: KeyboardEvent) {
  // Alt+Shift+Arrow → resize focused pane
  if (e.altKey && e.shiftKey && !e.ctrlKey) {
    const direction = arrowToDirection[e.code]
    if (!direction) return
    e.preventDefault()
    e.stopPropagation()
    const result = findResizableSplit(store.root, store.focusedId, direction)
    if (result) {
      store.adjustSplitSize(result.splitId, result.delta * 5)
    }
    return
  }

  // Ctrl+Shift+T → new workspace tab
  if (e.ctrlKey && e.shiftKey && e.code === 'KeyT') {
    e.preventDefault()
    e.stopPropagation()
    store.addWorkspace()
    return
  }

  // Ctrl+Tab / Ctrl+Shift+Tab → next/prev workspace
  if (e.ctrlKey && e.code === 'Tab') {
    e.preventDefault()
    e.stopPropagation()
    const count = store.workspaces.length
    if (count <= 1) return
    const delta = e.shiftKey ? -1 : 1
    const next = (store.activeWorkspaceIndex + delta + count) % count
    store.switchWorkspace(next)
    return
  }

  // Ctrl+1..9 → switch to workspace by number
  if (e.ctrlKey && !e.shiftKey && !e.altKey) {
    const digitMatch = e.code.match(/^Digit([1-9])$/)
    if (digitMatch) {
      const idx = parseInt(digitMatch[1], 10) - 1
      if (idx < store.workspaces.length) {
        e.preventDefault()
        e.stopPropagation()
        store.switchWorkspace(idx)
        return
      }
    }
  }

  if (!e.ctrlKey || !e.shiftKey) return

  // Ctrl+Shift+Arrow → navigate panes
  const direction = arrowToDirection[e.code]
  if (direction) {
    e.preventDefault()
    e.stopPropagation()
    const rects = computeLeafRects(store.root)
    const neighbor = findNeighbor(rects, store.focusedId, direction)
    if (neighbor) store.setFocus(neighbor)
    return
  }

  // Ctrl+Shift+O → workspace overview
  if (e.code === 'KeyO') {
    e.preventDefault()
    e.stopPropagation()
    toggleOverviewWindow()
    return
  }

  // Ctrl+Shift+R → split right (vertical, side by side)
  if (e.code === 'KeyR') {
    e.preventDefault()
    e.stopPropagation()
    store.splitFocused('vertical')
    return
  }

  // Ctrl+Shift+D → split down (horizontal, stacked)
  if (e.code === 'KeyD') {
    e.preventDefault()
    e.stopPropagation()
    store.splitFocused('horizontal')
    return
  }

  // Ctrl+Shift+W → close focused pane, or close workspace if last pane
  if (e.code === 'KeyW') {
    e.preventDefault()
    e.stopPropagation()
    const currentWs = store.workspaces[store.activeWorkspaceIndex]
    if (currentWs.type === 'terminal' && store.root.type === 'terminal' && store.workspaces.length > 1) {
      confirmCloseWorkspace(store.activeWorkspaceIndex)
    } else {
      store.closeFocused()
    }
  }
}

const { confirm: confirmDialog } = useConfirm()

async function confirmCloseWorkspace(index: number) {
  const ws = store.workspaces[index]
  if (!ws) return
  const ok = await confirmDialog({
    title: `Close workspace "${ws.name}"?`,
    message: ws.type === 'project'
      ? 'All terminals in this project workspace will be closed.'
      : 'All terminals in this workspace will be closed.',
    confirmText: 'Close',
    danger: true,
  })
  if (ok) store.removeWorkspace(index)
}

// ── Settings menu ────────────────────────────────────────────────────────────

const settingsOpen = ref(false)
const shortcutsOpen = ref(false)

function toggleOverviewWindow() {
  overviewOpen.value = !overviewOpen.value
  invoke(overviewOpen.value ? 'show_overview_window' : 'hide_overview_window')
}

function resetOverviewWindow(e: MouseEvent) {
  e.preventDefault()
  invoke('reset_overview_window', { toDefault: e.shiftKey })
  overviewOpen.value = true
}

// ── Drag and drop ────────────────────────────────────────────────────────────

let unlistenDragDrop: (() => void) | null = null
let unlistenOverviewRequest: (() => void) | null = null
let unlistenOverviewNavigate: (() => void) | null = null
let unlistenOverviewClosed: (() => void) | null = null

async function setupDragDrop() {
  const webview = getCurrentWebview()
  unlistenDragDrop = await webview.onDragDropEvent((event) => {
    if (event.payload.type !== 'drop') return
    const paths = (event.payload as any).paths as string[]
    if (!paths?.length) return

    // Position is in physical pixels — convert to logical for elementFromPoint
    const dpr = window.devicePixelRatio || 1
    const x = event.payload.position.x / dpr
    const y = event.payload.position.y / dpr

    // Find which terminal pane the drop landed on
    const el = document.elementFromPoint(x, y)
    const paneEl = el?.closest('.terminal-pane') as HTMLElement | null
    if (!paneEl) return

    // Get the pane ID from the data attribute
    const paneId = paneEl.dataset.paneId
    if (!paneId) return

    // Write paths to the pane's PTY session
    const ptySessionId = store.getPtySession(paneId)
    if (!ptySessionId) return

    const quoted = paths.map(p => p.includes(' ') ? `"${p}"` : p)
    invoke('write_to_session', { sessionId: ptySessionId, data: quoted.join(' ') })
    store.setFocus(paneId)
  })
}

function handleContextMenu(e: MouseEvent) {
  e.preventDefault()
}

onMounted(async () => {
  window.addEventListener('keydown', handleKeyDown, { capture: true })
  window.addEventListener('contextmenu', handleContextMenu)

  // Set up overview listeners before loadAndRestore, which may show the overview window
  unlistenOverviewRequest = await listen('overview-request-update', () => {
    store.emitOverviewUpdate()
  }) as unknown as (() => void)
  unlistenOverviewNavigate = await listen<{ workspaceIndex: number; paneId: string }>('overview-navigate', (event) => {
    store.switchWorkspace(event.payload.workspaceIndex)
    store.setFocus(event.payload.paneId)
    store.triggerFocus()
    getCurrentWindow().setFocus()
  }) as unknown as (() => void)
  unlistenOverviewClosed = await listen('overview-closed', () => {
    overviewOpen.value = false
    // No need to invoke hide — the overview window hid itself
  }) as unknown as (() => void)

  await loadAndRestore()
  ready.value = true
  // Fetch project models and set up refs watchers (panes were already
  // registered synchronously inside loadAndRestore).
  useProjectStore().initAllProjectWorkspaces()
  await setupCloseHandler()
  await setupDragDrop()

  // Push terminal data to overview window after everything is initialized.
  // The overview WebView loads asynchronously, so also push after a delay
  // to cover the case where it mounts after this point.
  if (overviewOpen.value) {
    store.emitOverviewUpdate()
    setTimeout(() => store.emitOverviewUpdate(), 1000)
  }

  // WebView2 on Windows has a separate internal focus from the Win32 window.
  // MoveFocus(PROGRAMMATIC) via Rust pushes focus into the web content layer,
  // after which JS .focus() on the xterm textarea actually works.
  setTimeout(async () => {
    await invoke('focus_webview')
    // Now that WebView2 content has native focus, focus the xterm textarea
    const pane = document.querySelector('.terminal-pane.focused')
    const textarea = pane?.querySelector('textarea') as HTMLTextAreaElement | null
    textarea?.focus()
  }, 200)
})
onBeforeUnmount(() => {
  window.removeEventListener('keydown', handleKeyDown, { capture: true })
  window.removeEventListener('contextmenu', handleContextMenu)
  unlistenDragDrop?.()
  unlistenOverviewRequest?.()
  unlistenOverviewNavigate?.()
  unlistenOverviewClosed?.()
})
</script>

<template>
  <div class="app">
    <div class="titlebar">
      <div class="titlebar-brand">
        <img :src="logoUrl" class="titlebar-logo" alt="Arbiter" />
        <span class="titlebar-title">Arbiter</span>
      </div>
      <WorkspaceTabs v-if="ready" />
      <div class="titlebar-right">
        <StatsBar v-if="!devStore.hideUsageBar" />
        <button class="settings-btn" :class="{ active: overviewOpen }" title="Workspace overview (Ctrl+Shift+O)" @click="toggleOverviewWindow()" @contextmenu="resetOverviewWindow">
          <MdiIcon :path="mdiViewDashboardOutline" :size="16" />
        </button>
        <button class="settings-btn" title="Keyboard shortcuts" @click="shortcutsOpen = true">
          <MdiIcon :path="mdiKeyboardOutline" :size="16" />
        </button>
        <button class="settings-btn" title="Settings" @click="settingsOpen = true">
          <MdiIcon :path="mdiCogOutline" :size="16" />
        </button>
      </div>
      <WindowControls />
    </div>
    <div v-if="ready" class="workspace">
      <ProjectWorkspaceView
        v-if="store.workspaces[store.activeWorkspaceIndex].type === 'project'"
        :workspace="store.workspaces[store.activeWorkspaceIndex] as any"
        :key="store.workspaces[store.activeWorkspaceIndex].id"
      />
      <SplitView
        v-else
        :node="store.root"
        :key="store.workspaces[store.activeWorkspaceIndex].id"
      />
    </div>

    <ShortcutsDialog v-if="shortcutsOpen" @close="shortcutsOpen = false" />
    <SettingsDialog v-if="settingsOpen" @close="settingsOpen = false" />

    <ConfirmDialog />
  </div>
</template>

<style scoped>
.app {
  display: flex;
  flex-direction: column;
  height: 100vh;
  width: 100vw;
}

.titlebar {
  height: 44px;
  background: var(--color-bg-subtle);
  border-bottom: 1px solid var(--color-card-border);
  display: grid;
  grid-template-columns: auto 1fr auto auto;
  align-items: center;
  padding: 0 0 0 6px;
  user-select: none;
  -webkit-app-region: drag;
  flex-shrink: 0;
}

.titlebar-brand {
  display: flex;
  align-items: center;
  gap: 5px;
  padding-right: 8px;
}

.titlebar-logo {
  width: 28px;
  height: 28px;
  flex-shrink: 0;
}

.titlebar-title {
  font-family: 'DM Sans', sans-serif;
  font-weight: 700;
  font-size: 15px;
  letter-spacing: 0.06em;
  background: linear-gradient(
    90deg,
    var(--azure-baby)    0%,
    var(--azure)         25%,
    var(--azure-deep)    50%,
    var(--azure-tropical) 75%,
    var(--azure-baby)    100%
  );
  background-size: 250% auto;
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
  animation: title-shimmer 6s ease-in-out infinite alternate;
}

@keyframes title-shimmer {
  from { background-position: 0% center; }
  to   { background-position: 100% center; }
}

.titlebar-right {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 0 8px;
  -webkit-app-region: no-drag;
}

.settings-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  background: none;
  border: 1px solid var(--color-card-border);
  border-radius: 4px;
  color: var(--color-text-muted);
  cursor: pointer;
  padding: 4px;
  line-height: 1;
  transition: color 0.15s, border-color 0.15s, background 0.15s;
}

.settings-btn:hover {
  color: var(--color-accent);
  border-color: var(--color-accent);
  background: var(--color-bg-elevated);
}

.settings-btn.active {
  color: var(--color-accent);
  border-color: var(--color-accent);
}

.workspace {
  flex: 1;
  min-height: 0;
  overflow: hidden;
  background: var(--color-bg);
  position: relative;
}
.workspace > * {
  height: 100%;
}
</style>
