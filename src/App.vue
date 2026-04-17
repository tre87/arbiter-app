<script setup lang="ts">
import { ref, watch, nextTick, onMounted, onBeforeUnmount, defineAsyncComponent } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { usePaneStore } from './stores/pane'
import { useProjectStore } from './stores/project'
import { useDevSettingsStore } from './stores/devSettings'
import SplitView from './components/SplitView.vue'
import ProjectWorkspaceView from './components/ProjectWorkspaceView.vue'
import StatsBar from './components/StatsBar.vue'
import ConfirmDialog from './components/ConfirmDialog.vue'
import MdiIcon from './components/MdiIcon.vue'
import { mdiCogOutline, mdiKeyboardOutline, mdiViewDashboardOutline, mdiBugOutline } from '@mdi/js'
import WorkspaceTabs from './components/WorkspaceTabs.vue'
import WindowControls from './components/WindowControls.vue'
import logoUrl from './assets/logo.svg'
import { useAutosave } from './composables/useAutosave'
import { loadAndRestore } from './composables/useStartupRestore'
import { useKeyboardShortcuts } from './composables/useKeyboardShortcuts'
import { useTitlebarDrag, useWindowChrome } from './composables/useWindowChrome'

// Lazy-loaded: these dialogs are only needed when the user opens them,
// so they don't belong in the initial bundle.
const ShortcutsDialog = defineAsyncComponent(() => import('./components/ShortcutsDialog.vue'))
const SettingsDialog = defineAsyncComponent(() => import('./components/SettingsDialog.vue'))

const store = usePaneStore()
const devStore = useDevSettingsStore()
const ready = ref(false)
const overviewOpen = ref(false)
const settingsOpen = ref(false)
const shortcutsOpen = ref(false)

const isMac = typeof navigator !== 'undefined' && navigator.platform.startsWith('Mac')
const isWindows = typeof navigator !== 'undefined' && navigator.platform.startsWith('Win')
if (typeof document !== 'undefined') {
  if (isMac) document.body.classList.add('is-macos')
  else if (isWindows) document.body.classList.add('is-windows')
  else document.body.classList.add('is-linux')
}

const { flush: flushAutosave } = useAutosave(ready, overviewOpen)

// Panes in newly-active workspaces need a chance to refit: ResizeObserver
// doesn't fire while an ancestor is `display: none`, so a window resize
// during backgrounding wouldn't have reached them. Each TerminalPane decides
// whether to refit based on its own visibility.
watch(() => store.activeWorkspaceIndex, async () => {
  await nextTick()
  requestAnimationFrame(() => {
    window.dispatchEvent(new Event('arbiter:workspace-activated'))
  })
})

function toggleOverviewWindow() {
  overviewOpen.value = !overviewOpen.value
  invoke(overviewOpen.value ? 'show_overview_window' : 'hide_overview_window')
}

function resetOverviewWindow(e: MouseEvent) {
  e.preventDefault()
  invoke('reset_overview_window', { toDefault: e.shiftKey })
  overviewOpen.value = true
}

useKeyboardShortcuts(toggleOverviewWindow)
const { onTitlebarMouseDown, onTitlebarDblClick } = useTitlebarDrag()
useWindowChrome()

// Close handler: state is autosaved continuously, so just flush and exit.
async function setupCloseHandler() {
  const win = getCurrentWindow()
  await win.onCloseRequested(async () => {
    try { await flushAutosave() } catch { /* best-effort */ }
    await invoke('exit_app')
  })
}

let unlistenOverviewRequest: (() => void) | null = null
let unlistenOverviewNavigate: (() => void) | null = null
let unlistenOverviewClosed: (() => void) | null = null
let unlistenOverviewReorder: (() => void) | null = null

onMounted(async () => {
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
  unlistenOverviewReorder = await listen<{ from: number; to: number }>('overview-reorder-workspace', (event) => {
    store.moveWorkspace(event.payload.from, event.payload.to)
    store.emitOverviewUpdate()
  }) as unknown as (() => void)
  unlistenOverviewClosed = await listen('overview-closed', () => {
    overviewOpen.value = false
  }) as unknown as (() => void)

  await loadAndRestore(overviewOpen)
  ready.value = true
  useProjectStore().initAllProjectWorkspaces()
  await setupCloseHandler()

  // Push terminal data to overview window after initialization. The overview
  // WebView loads asynchronously; push again after a delay to cover the case
  // where it mounts after this point.
  if (overviewOpen.value) {
    store.emitOverviewUpdate()
    setTimeout(() => store.emitOverviewUpdate(), 1000)
  }

  // WebView2 on Windows has a separate internal focus from the Win32 window.
  // MoveFocus(PROGRAMMATIC) via Rust pushes focus into the web content layer,
  // after which JS .focus() on the xterm textarea actually works.
  setTimeout(async () => {
    await invoke('focus_webview')
    const pane = document.querySelector('.terminal-pane.focused')
    const textarea = pane?.querySelector('textarea') as HTMLTextAreaElement | null
    textarea?.focus()
  }, 200)
})

onBeforeUnmount(() => {
  unlistenOverviewRequest?.()
  unlistenOverviewNavigate?.()
  unlistenOverviewReorder?.()
  unlistenOverviewClosed?.()
})
</script>

<template>
  <div class="app">
    <div class="titlebar" @mousedown="onTitlebarMouseDown" @dblclick="onTitlebarDblClick">
      <div class="titlebar-brand">
        <img :src="logoUrl" class="titlebar-logo" alt="Arbiter" />
        <span class="titlebar-title">Arbiter</span>
      </div>
      <WorkspaceTabs v-if="ready" />
      <div v-if="!devStore.hideUsageBar" class="titlebar-stats">
        <StatsBar />
      </div>
      <div class="titlebar-actions">
        <button class="btn-icon" :class="{ 'is-active': overviewOpen }" title="Workspace overview (Ctrl+Shift+O)" @click="toggleOverviewWindow()" @contextmenu="resetOverviewWindow">
          <MdiIcon :path="mdiViewDashboardOutline" :size="16" />
        </button>
        <button class="btn-icon" title="Keyboard shortcuts" @click="shortcutsOpen = true">
          <MdiIcon :path="mdiKeyboardOutline" :size="16" />
        </button>
        <button class="btn-icon" title="DevTools" @click="invoke('open_devtools')">
          <MdiIcon :path="mdiBugOutline" :size="16" />
        </button>
        <button class="btn-icon" title="Settings" @click="settingsOpen = true">
          <MdiIcon :path="mdiCogOutline" :size="16" />
        </button>
      </div>
      <WindowControls v-if="!isMac" />
    </div>
    <template v-if="ready">
      <div
        v-for="(ws, i) in store.workspaces"
        :key="ws.id"
        v-show="i === store.activeWorkspaceIndex"
        class="workspace"
      >
        <ProjectWorkspaceView
          v-if="ws.type === 'project'"
          :workspace="ws as any"
        />
        <div v-else class="terminal-workspace">
          <div class="panel-card terminal-workspace-card">
            <SplitView :node="(ws as any).root" />
          </div>
        </div>
      </div>
    </template>

    <ShortcutsDialog v-if="shortcutsOpen" @close="shortcutsOpen = false" />
    <SettingsDialog v-if="settingsOpen" @close="settingsOpen = false" />

    <ConfirmDialog />
  </div>
</template>

<style scoped>
.app {
  position: relative;
  display: flex;
  flex-direction: column;
  height: 100vh;
  width: 100vw;
  background: var(--color-bg);
}

.app::before {
  content: '';
  position: absolute;
  inset: 0;
  background: radial-gradient(
    ellipse 900px 560px at calc(var(--titlebar-pad-left) + 14px) 0,
    rgba(51, 153, 255, 0.32) 0%,
    rgba(51, 153, 255, 0.16) 25%,
    rgba(51, 153, 255, 0.06) 55%,
    transparent 85%
  );
  pointer-events: none;
  z-index: 0;
}

.app > * {
  position: relative;
  z-index: 1;
}

.titlebar {
  height: var(--titlebar-height);
  background: transparent;
  display: flex;
  align-items: center;
  padding: 0 var(--titlebar-pad-right) 0 var(--titlebar-pad-left);
  user-select: none;
  flex-shrink: 0;
  min-width: 0;
}

.titlebar-brand {
  display: flex;
  align-items: center;
  gap: 5px;
  padding-right: 8px;
  flex: 0 0 auto;
}

.titlebar :deep(.workspace-tabs) {
  flex: 1 1 0;
  min-width: 0;
}

.titlebar-stats {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 0 8px;
  flex: 0 1 auto;
  min-width: 0;
  overflow: hidden;
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

.titlebar-actions {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 0 8px 0 0;
  flex: 0 0 auto;
}

.workspace {
  flex: 1;
  min-height: 0;
  overflow: hidden;
  background: transparent;
  position: relative;
}
.workspace > * {
  height: 100%;
}

.terminal-workspace {
  display: flex;
  padding: 0 var(--workspace-padding) var(--workspace-padding);
  background: transparent;
}

.terminal-workspace-card {
  flex: 1;
  min-width: 0;
  min-height: 0;
  background: var(--color-bg-subtle);
  border: 1px solid var(--color-card-border);
  border-radius: var(--radius-lg);
  overflow: hidden;
  box-shadow: var(--panel-shadow);
  display: flex;
  flex-direction: column;
}
.terminal-workspace-card > * {
  flex: 1;
  min-height: 0;
}
</style>
