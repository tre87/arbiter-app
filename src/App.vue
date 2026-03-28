<script setup lang="ts">
import { ref, onMounted, onBeforeUnmount } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import { usePaneStore } from './stores/pane'
import SplitView from './components/SplitView.vue'
import StatsBar from './components/StatsBar.vue'
import CloseDialog from './components/CloseDialog.vue'
import MdiIcon from './components/MdiIcon.vue'
import { mdiCogOutline, mdiKeyboardOutline } from '@mdi/js'
import ShortcutsDialog from './components/ShortcutsDialog.vue'
import SettingsDialog from './components/SettingsDialog.vue'
import logoUrl from './assets/logo.svg'
import { computeLeafRects, findNeighbor, findResizableSplit, type Direction } from './utils/spatial'
import type { ArbiterConfig, CloseOptions, SavedTerminal } from './types/config'

const store = usePaneStore()
const showCloseDialog = ref(false)
const closeOptions = ref<CloseOptions>({ saveLayout: true, savePaths: true, saveSessions: true })

// ── Startup: load config and restore layout ──────────────────────────────────

async function loadAndRestore() {
  try {
    const config = await invoke<ArbiterConfig | null>('load_config')
    if (!config) return

    // Restore close dialog checkbox states
    if (config.closeOptions) {
      closeOptions.value = { ...config.closeOptions }
    }

    // Restore window geometry — wait for the OS to actually apply the resize
    if (config.window) {
      const win = getCurrentWindow()
      try {
        await win.setSize(new (await import('@tauri-apps/api/dpi')).LogicalSize(config.window.width, config.window.height))
        await win.setPosition(new (await import('@tauri-apps/api/dpi')).LogicalPosition(config.window.x, config.window.y))
        // OS window resize is async; give it time to propagate to the DOM
        await new Promise(r => setTimeout(r, 150))
      } catch { /* ignore if position is off-screen */ }
    }

    // Restore layout
    if (config.layout && config.terminals) {
      store.restoreFromSaved(config.layout, config.terminals, config.focusedTerminalIndex)
    }
  } catch {
    // Config load failed — start fresh
  }
}

// ── Close intercept ──────────────────────────────────────────────────────────

async function setupCloseHandler() {
  const win = getCurrentWindow()
  await win.onCloseRequested(async (event) => {
    // Single terminal — nothing worth saving, just exit
    if (store.root.type === 'terminal') {
      return
    }
    event.preventDefault()
    showCloseDialog.value = true
  })
}

async function handleCloseConfirm(saveLayout: boolean, savePaths: boolean, saveSessions: boolean) {
  showCloseDialog.value = false
  closeOptions.value = { saveLayout, savePaths, saveSessions }

  try {
    const config: ArbiterConfig = {
      closeOptions: { saveLayout, savePaths, saveSessions },
    }

    if (saveLayout) {
      // Save window geometry
      const win = getCurrentWindow()
      try {
        const size = await win.outerSize()
        const pos = await win.outerPosition()
        config.window = { width: size.width, height: size.height, x: pos.x, y: pos.y }
      } catch { /* ignore */ }

      // Save layout tree
      const { layout, terminals: terminalMeta, focusedTerminalIndex } = store.serializeLayout()
      const savedTerminals: SavedTerminal[] = []

      for (const t of terminalMeta) {
        const entry: SavedTerminal = { name: t.name }

        if (savePaths) {
          const sessionId = store.getPtySession(t.id)
          if (sessionId) {
            try {
              const cwd = await invoke<string | null>('get_session_cwd', { sessionId })
              if (cwd) entry.cwd = cwd
            } catch { /* ignore */ }
          }
        }

        if (saveSessions) {
          const claudeId = store.getClaudeSessionId(t.id)
          if (claudeId) entry.claudeSessionId = claudeId
          if (store.isClaudeRunning(t.id)) entry.claudeWasRunning = true
        }

        savedTerminals.push(entry)
      }

      config.layout = layout
      config.terminals = savedTerminals
      config.focusedTerminalIndex = focusedTerminalIndex
    }

    await invoke('save_config', { config })
  } catch (e) {
    console.error('Failed to save config:', e)
  }

  // Exit the app via Rust — guaranteed to work
  await invoke('exit_app')
}

function handleCloseCancel() {
  showCloseDialog.value = false
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

  // Ctrl+Shift+W → close focused pane
  if (e.code === 'KeyW') {
    e.preventDefault()
    e.stopPropagation()
    store.closeFocused()
  }
}

// ── Settings menu ────────────────────────────────────────────────────────────

const settingsOpen = ref(false)
const shortcutsOpen = ref(false)

// ── Drag and drop ────────────────────────────────────────────────────────────

let unlistenDragDrop: (() => void) | null = null

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

onMounted(async () => {
  window.addEventListener('keydown', handleKeyDown, { capture: true })
  await loadAndRestore()
  await setupCloseHandler()
  await setupDragDrop()

  // Dispatch custom event so the focused TerminalPane focuses its own xterm.
  // Poll because terminals mount asynchronously after layout restore.
  let focusAttempts = 0
  const focusInterval = setInterval(() => {
    window.dispatchEvent(new Event('arbiter:request-focus'))
    // Verify: check if any textarea inside the focused pane got focus
    const pane = document.querySelector('.terminal-pane.focused')
    const textarea = pane?.querySelector('textarea')
    if (textarea && document.activeElement === textarea) {
      clearInterval(focusInterval)
      return
    }
    if (++focusAttempts >= 30) clearInterval(focusInterval) // give up after 3s
  }, 100)
})
onBeforeUnmount(() => {
  window.removeEventListener('keydown', handleKeyDown, { capture: true })
  unlistenDragDrop?.()
})
</script>

<template>
  <div class="app">
    <div class="titlebar">
      <div class="titlebar-brand">
        <img :src="logoUrl" class="titlebar-logo" alt="Arbiter" />
        <span class="titlebar-title">Arbiter</span>
      </div>
      <div class="titlebar-center">
        <StatsBar />
      </div>
      <div class="titlebar-actions">
        <button class="settings-btn" title="Keyboard shortcuts" @click="shortcutsOpen = true">
          <MdiIcon :path="mdiKeyboardOutline" :size="16" />
        </button>
        <button class="settings-btn" title="Settings" @click="settingsOpen = true">
          <MdiIcon :path="mdiCogOutline" :size="16" />
        </button>
      </div>
    </div>
    <div class="workspace">
      <SplitView :node="store.root" />
    </div>

    <ShortcutsDialog v-if="shortcutsOpen" @close="shortcutsOpen = false" />
    <SettingsDialog v-if="settingsOpen" @close="settingsOpen = false" />

    <CloseDialog
      v-if="showCloseDialog"
      :initial-save-layout="closeOptions.saveLayout"
      :initial-save-paths="closeOptions.savePaths"
      :initial-save-sessions="closeOptions.saveSessions"
      @confirm="handleCloseConfirm"
      @cancel="handleCloseCancel"
    />
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
  height: 46px;
  background: var(--color-bg-subtle);
  border-bottom: 1px solid var(--color-card-border);
  display: grid;
  grid-template-columns: auto 1fr auto;
  align-items: center;
  padding: 0 12px 0 6px;
  user-select: none;
  -webkit-app-region: drag;
  flex-shrink: 0;
}

.titlebar-center {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 10px;
  -webkit-app-region: no-drag;
}

.titlebar-actions {
  display: flex;
  align-items: center;
  gap: 6px;
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


.titlebar-brand {
  display: flex;
  align-items: center;
  gap: 5px;
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

.workspace {
  flex: 1;
  overflow: hidden;
  background: var(--color-bg);
}
</style>
