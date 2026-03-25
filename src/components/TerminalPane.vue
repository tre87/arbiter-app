<script setup lang="ts">
import { ref, computed, watch, onMounted, onBeforeUnmount } from 'vue'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import { WebLinksAddon } from '@xterm/addon-web-links'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { usePaneStore } from '../stores/pane'
import ClaudeIcon from './ClaudeIcon.vue'

const props = defineProps<{ paneId: string }>()
const store = usePaneStore()
const terminalEl = ref<HTMLDivElement>()
const isFocused = computed(() => store.focusedId === props.paneId)

let term: Terminal
let fitAddon: FitAddon
let unlisten: UnlistenFn | null = null
let sessionId: string | null = null
let resizeObserver: ResizeObserver | null = null
let fitTimer: ReturnType<typeof setTimeout> | null = null

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
    lineHeight: 1.4,
    cursorBlink: true,
    cursorStyle: 'bar',
    scrollback: 5000,
    allowTransparency: true,
  })

  fitAddon = new FitAddon()
  term.loadAddon(fitAddon)
  term.loadAddon(new WebLinksAddon())
  term.open(terminalEl.value!)
  // Defer until after the flex layout has settled
  await new Promise<void>(r => requestAnimationFrame(() => requestAnimationFrame(() => r())))
  fitAddon.fit()

  term.textarea?.addEventListener('focus', () => store.setFocus(props.paneId))

  // Wire up resize BEFORE creating the session so no event is missed
  term.onResize(({ cols, rows }) => {
    if (sessionId) invoke('resize_session', { sessionId, cols, rows })
  })

  // Create PTY with the actual fitted dimensions so it matches xterm from the start
  sessionId = await invoke<string>('create_session', { cols: term.cols, rows: term.rows })

  unlisten = await listen<string>(`pty-output-${sessionId}`, (event) => {
    term.write(event.payload)
  })

  term.onData((data) => {
    if (sessionId) invoke('write_to_session', { sessionId, data })
  })

  resizeObserver = new ResizeObserver(scheduleFit)
  resizeObserver.observe(terminalEl.value!)

  if (isFocused.value) term.focus()
})

onBeforeUnmount(() => {
  if (fitTimer) clearTimeout(fitTimer)
  unlisten?.()
  resizeObserver?.disconnect()
  if (sessionId) invoke('close_session', { sessionId })
  term?.dispose()
})
</script>

<template>
  <div class="terminal-pane" :class="{ focused: isFocused }" @mousedown="store.setFocus(paneId)">
    <div class="pane-toolbar">
      <button class="toolbar-btn claude-btn" title="Launch claude" @click="launchClaude" @mousedown.stop>
        <ClaudeIcon :size="14" />
      </button>
      <button class="toolbar-btn claude-btn continue-btn" title="claude --continue" @click="continueClaude" @mousedown.stop>
        <ClaudeIcon :size="14" />
        <span class="continue-arrow">&gt;&gt;</span>
      </button>
    </div>
    <div ref="terminalEl" class="terminal-inner" />
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

.toolbar-btn {
  display: flex;
  align-items: center;
  gap: 5px;
  background: none;
  border: 1px solid var(--color-card-border);
  border-radius: 3px;
  color: var(--color-text-muted);
  cursor: pointer;
  font-size: 11px;
  font-family: inherit;
  padding: 2px 6px;
  line-height: 1;
  transition: color 0.15s, border-color 0.15s, background 0.15s;
  user-select: none;
}

.toolbar-btn:hover {
  background: var(--color-bg-elevated);
  border-color: var(--color-card-border);
  color: var(--color-text-primary);
}

.claude-btn:hover {
  border-color: #D9775744;
}

.continue-arrow {
  font-size: 12px;
  color: var(--color-text-muted);
  line-height: 1;
}

.claude-btn:hover .continue-arrow {
  color: #D97757;
}

.terminal-inner {
  flex: 1;
  overflow: hidden;
}

/* Add breathing room between characters and the focus border
   without touching FitAddon's container measurement */
.terminal-inner :deep(.xterm-screen) {
  margin: 2px;
}
</style>
