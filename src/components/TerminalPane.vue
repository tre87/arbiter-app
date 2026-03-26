<script setup lang="ts">
import { ref, computed, watch, onMounted, onBeforeUnmount } from 'vue'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import { WebLinksAddon } from '@xterm/addon-web-links'
import { CanvasAddon } from '@xterm/addon-canvas'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { usePaneStore } from '../stores/pane'
import MdiIcon from './MdiIcon.vue'
import ClaudeIcon from './ClaudeIcon.vue'
import { mdiInformationOutline, mdiChevronDoubleRight } from '@mdi/js'
import TerminalFooter from './TerminalFooter.vue'

const props = defineProps<{ paneId: string }>()
const store = usePaneStore()
const terminalEl = ref<HTMLDivElement>()
const isFocused = computed(() => store.focusedId === props.paneId)

let term: Terminal
let fitAddon: FitAddon
let canvasAddon: CanvasAddon | null = null
let unlisten: UnlistenFn | null = null
let sessionId: string | null = null
let resizeObserver: ResizeObserver | null = null
let fitTimer: ReturnType<typeof setTimeout> | null = null

// ── Claude detection state ───────────────────────────────────────────────────
const claudeRunning = ref(false)
const footerVisible = ref(true)
const claudeStatus = ref<{
  session_id: string
  model_id?: string | null
  input_tokens?: number | null
  output_tokens?: number | null
  cache_creation_input_tokens?: number | null
  cache_read_input_tokens?: number | null
  folder?: string | null
  branch?: string | null
} | null>(null)

const infoPanelOpen = ref(false)

function toggleInfoPanel() {
  infoPanelOpen.value = !infoPanelOpen.value
}

// Unlisten callbacks for Rust-emitted Claude lifecycle events
let unlistenStarted: (() => void) | null = null
let unlistenStatus:  (() => void) | null = null
let unlistenExited:  (() => void) | null = null

// ── Toolbar actions ───────────────────────────────────────────────────────────

function scheduleFit() {
  if (fitTimer) clearTimeout(fitTimer)
  fitTimer = setTimeout(() => fitAddon?.fit(), 50)
}

watch(isFocused, (focused) => {
  if (focused) term?.focus()
})

function launchClaude() {
  if (sessionId) {
    invoke('write_to_session', { sessionId, data: 'claude\r' })
    term?.focus()
  }
}

function continueClaude() {
  if (sessionId) {
    invoke('write_to_session', { sessionId, data: 'claude --continue\r' })
    term?.focus()
  }
}

function modelLabel(id: string | null | undefined): string {
  if (!id) return ''
  const m = id.match(/(opus|sonnet|haiku|flash)[- ]?(\d+)[- ]?(\d+)?/)
  if (m) {
    const family = m[1].charAt(0).toUpperCase() + m[1].slice(1)
    const ver = m[3] ? `${m[2]}.${m[3]}` : m[2]
    return `${family} ${ver}`
  }
  return id.replace('claude-', '')
}

// ── Lifecycle ─────────────────────────────────────────────────────────────────

onMounted(async () => {
  term = new Terminal({
    theme: {
      background: '#121212',
      foreground: '#e8eaed',
      cursor: '#3399FF',
      selectionBackground: 'rgba(51,153,255,0.25)',
      black: '#1e1e1e',
      brightBlack: '#555',
      red: '#f44747',     brightRed: '#f44747',
      green: '#6a9955',   brightGreen: '#b5cea8',
      yellow: '#d7ba7d',  brightYellow: '#d7ba7d',
      blue: '#569cd6',    brightBlue: '#9cdcfe',
      magenta: '#c678dd', brightMagenta: '#c678dd',
      cyan: '#4ec9b0',    brightCyan: '#4ec9b0',
      white: '#d4d4d4',   brightWhite: '#ffffff',
    },
    fontFamily: "Consolas, 'Cascadia Code', Menlo, 'SF Mono', monospace",
    fontSize: 12,
    lineHeight: 1.0,
    cursorBlink: true,
    cursorStyle: 'bar',
    scrollback: 5000,
    allowTransparency: true,
  })

  fitAddon = new FitAddon()
  term.loadAddon(fitAddon)
  term.loadAddon(new WebLinksAddon())
  term.open(terminalEl.value!)
  await new Promise<void>(r => requestAnimationFrame(() => requestAnimationFrame(() => r())))
  fitAddon.fit()

  // Canvas renderer: proper pixel-aligned glyph drawing without WebGL context limits
  try {
    canvasAddon = new CanvasAddon()
    term.loadAddon(canvasAddon)
  } catch {
    canvasAddon = null
  }

  term.textarea?.addEventListener('focus', () => store.setFocus(props.paneId))

  term.onResize(({ cols, rows }) => {
    if (sessionId) invoke('resize_session', { sessionId, cols, rows })
  })

  // Reuse existing PTY session if the pane survived a split/remount; otherwise create fresh
  const existingSession = store.getPtySession(props.paneId)
  if (existingSession) {
    sessionId = existingSession

    // Subscribe to live output BEFORE resize so we don't miss the shell redraw
    unlisten = await listen<string>(`pty-output-${sessionId}`, (event) => {
      term.write(event.payload)
    })

    // Resize the PTY to match the new container — the shell gets notified and redraws.
    // We intentionally skip replaying the raw buffer here: it was captured at the old
    // terminal width, so escape sequences (cursor positioning, line wraps) would render
    // as garbled text at the new width. The running process redraws after the resize.
    invoke('resize_session', { sessionId, cols: term.cols, rows: term.rows })

    // Restore footer if Claude was running before the remount
    const status = await invoke('get_active_claude_status', { sessionId }).catch(() => null) as typeof claudeStatus.value
    if (status) {
      claudeStatus.value = status
      claudeRunning.value = true
      footerVisible.value = true
    }
  } else {
    sessionId = await invoke<string>('create_session', { cols: term.cols, rows: term.rows })
    store.setPtySession(props.paneId, sessionId)

    unlisten = await listen<string>(`pty-output-${sessionId}`, (event) => {
      term.write(event.payload)
    })
  }

  term.onData((data) => {
    if (sessionId) invoke('write_to_session', { sessionId, data })
  })

  resizeObserver = new ResizeObserver(scheduleFit)
  resizeObserver.observe(terminalEl.value!)

  if (isFocused.value) term.focus()

  // Subscribe to Rust-emitted Claude lifecycle events for this pane
  unlistenStarted = await listen(`claude-started-${sessionId}`, (event) => {
    claudeStatus.value = event.payload as typeof claudeStatus.value
    claudeRunning.value = true
    footerVisible.value = true
  })
  unlistenStatus = await listen(`claude-status-${sessionId}`, (event) => {
    claudeStatus.value = event.payload as typeof claudeStatus.value
  })
  unlistenExited = await listen(`claude-exited-${sessionId}`, () => {
    claudeRunning.value = false
    infoPanelOpen.value = false
  })
})

onBeforeUnmount(() => {
  unlistenStarted?.()
  unlistenStatus?.()
  unlistenExited?.()
  if (fitTimer) clearTimeout(fitTimer)
  unlisten?.()
  resizeObserver?.disconnect()
  // Only close the PTY session if this pane has been removed from the layout tree.
  // During a split the pane node survives, so we keep the session alive for reconnection.
  if (sessionId && !store.hasPaneId(props.paneId)) {
    invoke('close_session', { sessionId })
    store.removePtySession(props.paneId)
  }
  term?.dispose()
})
</script>

<template>
  <div class="terminal-pane" :class="{ focused: isFocused }" @mousedown="store.setFocus(paneId)">
    <div class="pane-toolbar">
      <template v-if="!claudeRunning">
        <button class="toolbar-btn claude-btn" title="Launch claude" @click="launchClaude" @mousedown.stop>
          <ClaudeIcon :size="14" />
        </button>
        <button class="toolbar-btn claude-btn" title="claude --continue" @click="continueClaude" @mousedown.stop>
          <ClaudeIcon :size="14" />
          <MdiIcon :path="mdiChevronDoubleRight" :size="14" class="continue-icon" />
        </button>
      </template>
      <span class="toolbar-spacer" />
      <button
        v-if="claudeRunning"
        class="toolbar-btn info-btn"
        :class="{ active: infoPanelOpen }"
        title="Session info"
        @click="toggleInfoPanel"
        @mousedown.stop
      >
        <MdiIcon :path="mdiInformationOutline" :size="14" />
      </button>
    </div>

    <!-- Info panel overlay -->
    <div v-if="infoPanelOpen && claudeStatus" class="info-panel">
      <div class="info-row">
        <span class="info-label">Session ID</span>
        <span class="info-value id-value">{{ claudeStatus.session_id }}</span>
      </div>
      <div v-if="claudeStatus.model_id" class="info-row">
        <span class="info-label">Model</span>
        <span class="info-value">{{ modelLabel(claudeStatus.model_id) }}</span>
      </div>
      <div v-if="claudeStatus.folder" class="info-row">
        <span class="info-label">Folder</span>
        <span class="info-value">{{ claudeStatus.folder }}</span>
      </div>
      <div v-if="claudeStatus.branch" class="info-row">
        <span class="info-label">Branch</span>
        <span class="info-value">{{ claudeStatus.branch }}</span>
      </div>
      <div class="info-row">
        <span class="info-label">Tokens in</span>
        <span class="info-value">{{ claudeStatus.input_tokens?.toLocaleString() ?? '—' }}</span>
      </div>
      <div class="info-row">
        <span class="info-label">Tokens out</span>
        <span class="info-value">{{ claudeStatus.output_tokens?.toLocaleString() ?? '—' }}</span>
      </div>
      <div class="info-row">
        <span class="info-label">Cache write</span>
        <span class="info-value">{{ claudeStatus.cache_creation_input_tokens?.toLocaleString() ?? '—' }}</span>
      </div>
      <div class="info-row">
        <span class="info-label">Cache read</span>
        <span class="info-value">{{ claudeStatus.cache_read_input_tokens?.toLocaleString() ?? '—' }}</span>
      </div>
    </div>

    <div ref="terminalEl" class="terminal-inner" />
    <TerminalFooter v-if="claudeRunning && footerVisible" :status="claudeStatus" />
  </div>
</template>

<style scoped>
.terminal-pane {
  position: relative;
  display: flex;
  flex-direction: column;
  width: 100%;
  height: 100%;
  overflow: hidden;
  background: var(--color-bg);
}

.terminal-pane::after {
  content: '';
  position: absolute;
  inset: 0;
  border: 1px solid transparent;
  pointer-events: none;
  transition: border-color 0.12s;
  z-index: 10;
}

.terminal-pane.focused::after {
  border-color: var(--color-accent);
}

.pane-toolbar {
  display: flex;
  align-items: center;
  gap: 4px;
  height: 30px;
  padding: 0 6px;
  background: var(--color-bg-subtle);
  border-bottom: 1px solid var(--color-card-border);
  flex-shrink: 0;
  z-index: 1;
}

.toolbar-spacer { flex: 1; }

.claude-btn {
  gap: 2px;
}

.claude-btn:hover {
  border-color: #D9775744;
}

.continue-icon {
  color: var(--color-text-muted);
}

.claude-btn:hover .continue-icon {
  color: #D97757;
}

.toolbar-btn {
  display: flex;
  align-items: center;
  background: none;
  border: 1px solid var(--color-card-border);
  border-radius: 3px;
  color: var(--color-text-muted);
  cursor: pointer;
  padding: 2px 5px;
  line-height: 1;
  transition: color 0.15s, border-color 0.15s, background 0.15s;
  user-select: none;
}

.toolbar-btn:hover {
  background: var(--color-bg-elevated);
  color: var(--color-text-primary);
}

.info-btn {
  opacity: 0.5;
}

.info-btn:hover,
.info-btn.active {
  opacity: 1;
  border-color: var(--color-accent);
  color: var(--color-accent);
}

.info-panel {
  position: absolute;
  top: 31px;
  right: 6px;
  z-index: 20;
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-card-border);
  border-radius: 4px;
  padding: 8px 12px;
  font-family: Consolas, 'Cascadia Code', Menlo, 'SF Mono', monospace;
  font-size: 11px;
  min-width: 220px;
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
}

.info-row {
  display: flex;
  justify-content: space-between;
  gap: 16px;
  padding: 3px 0;
}

.info-row + .info-row {
  border-top: 1px solid var(--color-card-border);
}

.info-label {
  color: var(--color-text-muted);
  opacity: 0.7;
  white-space: nowrap;
}

.info-value {
  color: var(--color-text-primary);
  text-align: right;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.id-value {
  color: #D97757;
  font-weight: 600;
  letter-spacing: 0.3px;
  font-size: 10px;
}

.terminal-inner {
  flex: 1;
  overflow: hidden;
}

.terminal-inner :deep(.xterm-screen) {
  padding: 2px;
}
</style>
