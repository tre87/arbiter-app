<script setup lang="ts">
import { ref, computed, watch, nextTick, onMounted, onBeforeUnmount } from 'vue'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import { WebLinksAddon } from '@xterm/addon-web-links'
import { WebglAddon } from '@xterm/addon-webgl'
import { readText as clipboardRead, writeText as clipboardWrite } from '@tauri-apps/plugin-clipboard-manager'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { usePaneStore } from '../stores/pane'
import { useDevSettingsStore } from '../stores/devSettings'
import MdiIcon from './MdiIcon.vue'
import ClaudeIcon from './ClaudeIcon.vue'
import { mdiInformationOutline, mdiChevronDoubleRight, mdiPencilOutline, mdiBash, mdiPowershell } from '@mdi/js'
import TerminalFooter from './TerminalFooter.vue'

const props = withDefaults(defineProps<{ paneId: string; compact?: boolean }>(), { compact: false })
const store = usePaneStore()
const devSettings = useDevSettingsStore()
const terminalEl = ref<HTMLDivElement>()
const isFocused = computed(() => store.focusedId === props.paneId)
let term: Terminal
let fitAddon: FitAddon
let webglAddon: WebglAddon | null = null
let unlisten: UnlistenFn | null = null
let sessionId: string | null = null
let resizeObserver: ResizeObserver | null = null
let fitTimer: ReturnType<typeof setTimeout> | null = null
let focusHandler: (() => void) | null = null
let workspaceActivatedHandler: (() => void) | null = null

// ── Claude detection state ───────────────────────────────────────────────────
// All Claude lifecycle state is centralized in the pane store (claudePaneStates).
// Persistent event listeners in pane.ts handle claude-started/status/exited/bell.
// TerminalPane only reads state reactively for rendering.
const claudeState = computed(() => store.getClaudePaneState(props.paneId))
const claudeActive = computed(() => store.isClaudeActive(props.paneId))
const footerVisible = ref(true)

const sessionCwd = ref<string | null>(null)
const folderName = ref<string | null>(null)
const gitInfo = ref<{ is_repo: boolean; branch: string | null } | null>(null)
let unlistenCwd: (() => void) | null = null

const terminalTitle = ref('')

// True when this pane lives inside a project workspace. Project workspaces
// render git actions in the worktree sidebar, so the terminal footer should
// only appear when Claude is running (to show claude stats).
const inProjectWorkspace = computed(() => store.isPaneInProjectWorkspace(props.paneId))

// Derived booleans for template usage
const claudeWorking = computed(() => claudeState.value.lifecycle === 'working')

const infoPanelOpen = ref(false)

function toggleInfoPanel() {
  infoPanelOpen.value = !infoPanelOpen.value
}

// ── Shell switching (Windows only) ──────────────────────────────────────────
const isWindows = navigator.platform.startsWith('Win')
const gitBashPath = ref<string | null>(null)
const shellIdle = ref(false)
const currentShell = ref<'powershell' | 'gitbash'>('powershell')

// ── Inline name editing ──────────────────────────────────────────────────────
const isEditingName = ref(false)
const editNameValue = ref('')
const nameInputEl = ref<HTMLInputElement>()

const terminalName = computed(() => store.getTerminalName(props.paneId))

function startEditName() {
  editNameValue.value = terminalName.value
  isEditingName.value = true
  nextTick(() => {
    nameInputEl.value?.focus()
    nameInputEl.value?.select()
  })
}

function commitName() {
  const trimmed = editNameValue.value.trim()
  if (trimmed && trimmed !== terminalName.value) {
    store.setTerminalName(props.paneId, trimmed)
  }
  isEditingName.value = false
}

function cancelEditName() {
  isEditingName.value = false
}


// ── Toolbar actions ───────────────────────────────────────────────────────────

// Fit terminal cols/rows to container, bypassing FitAddon's circular css.cell.width.
function safeFit() {
  if (!term) return
  const core = (term as any)._core
  const dw: number | undefined = core?._renderService?.dimensions?.device?.cell?.width
  const dh: number | undefined = core?._renderService?.dimensions?.device?.cell?.height
  if (!dw || !dh) {
    fitAddon?.fit()
    return
  }

  const dpr = window.devicePixelRatio || 1
  const parent = term.element?.parentElement
  if (!parent) return

  const parentWidth = parseFloat(window.getComputedStyle(parent).width)
  const parentHeight = parseFloat(window.getComputedStyle(parent).height)
  if (!parentWidth || !parentHeight) return

  const viewportEl = term.element?.querySelector('.xterm-viewport') as HTMLElement | null
  const scrollbarWidth = viewportEl ? (viewportEl.offsetWidth - viewportEl.clientWidth) : 0

  // Account for padding on .terminal-inner
  const parentStyle = window.getComputedStyle(parent)
  const paddingLeft = parseFloat(parentStyle.paddingLeft) || 0
  const paddingRight = parseFloat(parentStyle.paddingRight) || 0

  const available = parentWidth - scrollbarWidth - paddingLeft - paddingRight
  let cols = Math.max(2, Math.floor(available / (dw / dpr)))
  const rows = Math.max(1, Math.floor(parentHeight / (dh / dpr)))

  // Verify canvas won't overflow
  while (cols > 2 && Math.round(dw * cols / dpr) > available) {
    cols--
  }

  if (term.cols !== cols || term.rows !== rows) {
    term.resize(cols, rows)
  }
}

function scheduleFit() {
  if (fitTimer) clearTimeout(fitTimer)
  fitTimer = setTimeout(safeFit, 50)
}

watch(isFocused, (focused) => {
  if (focused) term?.focus()
})

// shellIdle is driven purely by the shell-activity-{sid} event from the backend
// (see subscribeToSession). When Claude is running the shell-switch button is
// hidden regardless, so we just clear it on claudeActive transitions.
watch(claudeActive, (active) => {
  if (active) shellIdle.value = false
})

function launchClaude() {
  if (sessionId) {
    store.updateClaudePaneState(props.paneId, { lifecycle: 'launching', confirmed: false, sessionId: null })
    store.armClaudeListeners(props.paneId)
    footerVisible.value = true
    invoke('write_to_session', { sessionId, data: 'claude\r' })
    term?.focus()
  }
}

function continueClaude() {
  if (sessionId) {
    store.updateClaudePaneState(props.paneId, { lifecycle: 'launching', confirmed: false, sessionId: null })
    store.armClaudeListeners(props.paneId)
    footerVisible.value = true
    invoke('write_to_session', { sessionId, data: 'claude --continue\r' })
    term?.focus()
  }
}

// ── Event subscription helper ────────────────────────────────────────────────
// Claude lifecycle events (started/status/exited/bell/shell-activity) are
// handled by persistent listeners in pane.ts — they survive component
// unmount/remount. This function only subscribes to CWD changes.
async function subscribeToSession(sid: string) {
  unlistenCwd = await listen(`cwd-changed-${sid}`, (event) => {
    const data = event.payload as { cwd: string; folder: string | null; git: { is_repo: boolean; branch: string | null } }
    sessionCwd.value = data.cwd
    folderName.value = data.folder
    gitInfo.value = data.git
  }) as unknown as (() => void)

  // Recover CWD that may have been emitted before this listener was attached
  const currentCwd = await invoke<string | null>('get_session_cwd', { sessionId: sid }).catch(() => null)
  if (currentCwd && !sessionCwd.value) {
    sessionCwd.value = currentCwd
    folderName.value = currentCwd.replace(/\\/g, '/').split('/').filter(Boolean).pop() ?? null
    const git = await invoke<{ is_repo: boolean; branch: string | null }>('get_session_git_info', { cwd: currentCwd }).catch(() => null)
    if (git) gitInfo.value = git
  }
  if (isWindows && gitBashPath.value && !claudeActive.value) {
    const currentIdle = await invoke<boolean | null>('get_session_shell_idle', { sessionId: sid }).catch(() => null)
    if (currentIdle !== null && currentIdle !== undefined) {
      shellIdle.value = currentIdle
    }
  }
}

function unsubscribeAll() {
  unlisten?.(); unlisten = null
  unlistenCwd?.(); unlistenCwd = null
}

// ── Shell switching ──────────────────────────────────────────────────────────
async function switchShell() {
  if (!sessionId) return
  const cwd = sessionCwd.value

  // Tear down
  unsubscribeAll()
  await invoke('close_session', { sessionId })
  store.removePtySession(props.paneId)

  // Clear terminal
  term.clear()
  term.reset()

  // Toggle shell
  const newShell = currentShell.value === 'powershell' ? gitBashPath.value : null
  currentShell.value = newShell ? 'gitbash' : 'powershell'
  store.setTerminalShell(props.paneId, currentShell.value)

  // Create new session
  sessionId = await invoke<string>('create_session', {
    cols: term.cols,
    rows: term.rows,
    cwd: cwd ?? null,
    shell: newShell,
  })
  store.setPtySession(props.paneId, sessionId)

  // Re-subscribe
  unlisten = await listen<string>(`pty-output-${sessionId}`, (event) => {
    term.write(event.payload)
  })
  await subscribeToSession(sessionId)

  // shellIdle will be driven by the shell-activity event on the new session
  shellIdle.value = false
  term.focus()
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
  // ── Terminal setup (matches VS Code's xterm integration) ─────────────────────
  term = new Terminal({
    theme: {
      background: '#121212',
      foreground: '#e8eaed',
      cursor: '#aeafad',
      cursorAccent: '#000000',
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
    cursorBlink: false,
    cursorStyle: 'block',
    cursorInactiveStyle: 'outline',
    cursorWidth: 1,
    scrollback: 5000,
    allowTransparency: true,
  })

  fitAddon = new FitAddon()
  term.loadAddon(fitAddon)
  term.loadAddon(new WebLinksAddon())
  term.open(terminalEl.value!)

  // Store OSC 0 title for display in toolbar (no Claude detection — that's
  // handled by persistent listeners in pane.ts via JSONL/process monitoring).
  term.parser.registerOscHandler(0, (data) => {
    terminalTitle.value = data
    return false
  })

  // Any user input clears the attention flag for this terminal.
  term.onData(() => {
    if (claudeState.value.lifecycle === 'attention') {
      store.updateClaudePaneState(props.paneId, { lifecycle: 'ready' })
    }
  })

  // Register focus handler immediately so App.vue's polling can reach us
  focusHandler = () => { if (isFocused.value) term?.focus() }
  window.addEventListener('arbiter:request-focus', focusHandler)

  // Refit when this pane's workspace becomes the active one. Background
  // workspaces render with `display: none` so ResizeObserver doesn't fire
  // while they're hidden — if the window resized in the interim we need a
  // one-shot refit on reveal. The offsetParent check ensures panes in
  // other still-hidden workspaces stay idle. If dimensions haven't
  // changed, safeFit no-ops and no PTY resize is issued.
  workspaceActivatedHandler = () => {
    if (terminalEl.value?.offsetParent !== null) scheduleFit()
  }
  window.addEventListener('arbiter:workspace-activated', workspaceActivatedHandler)

  await new Promise<void>(r => requestAnimationFrame(() => requestAnimationFrame(() => r())))

  // WebGL renderer (same as VS Code)
  try {
    webglAddon = new WebglAddon()
    term.loadAddon(webglAddon)
    webglAddon.onContextLoss(() => {
      webglAddon?.dispose()
      webglAddon = null
    })
  } catch (e) {
    console.warn('WebGL addon failed, using DOM renderer:', e)
    webglAddon = null
  }

  // Fit after renderer addon so dimensions match actual rendering
  safeFit()

  term.textarea?.addEventListener('focus', () => store.setFocus(props.paneId))

  term.onResize(({ cols, rows }) => {
    store.markResize(props.paneId)
    if (sessionId) invoke('resize_session', { sessionId, cols, rows })
  })

  // Detect Git Bash early so default shell choice works for new sessions
  if (isWindows) {
    gitBashPath.value = await invoke<string | null>('check_git_bash')
  }

  // Reuse existing PTY session if the pane survived a split/remount; otherwise create fresh
  const existingSession = store.getPtySession(props.paneId)
  if (existingSession) {
    sessionId = existingSession
    currentShell.value = store.getTerminalShell(props.paneId)

    // Subscribe to live output BEFORE resize so we don't miss the shell redraw
    unlisten = await listen<string>(`pty-output-${sessionId}`, (event) => {
      term.write(event.payload)
    })

    // Tell the PTY our current dimensions. The backend skips the actual resize
    // (and SIGWINCH) when dimensions are unchanged — this avoids Claude's Ink
    // TUI redrawing and producing ghost cursor artefacts on worktree switch.
    // When dimensions *did* change (e.g. after a split), SIGWINCH fires and
    // the running process redraws at the new size.
    store.markResize(props.paneId)
    const resized = await invoke<boolean>('resize_session', { sessionId, cols: term.cols, rows: term.rows })
    if (!resized) {
      // Dimensions unchanged (worktree switch) — replay the output buffer to
      // restore terminal content without triggering a TUI redraw.
      const replay = await invoke<string>('get_session_replay', { sessionId })
      if (replay) term.write(replay)
    }

    // Claude state was maintained by persistent listeners in pane.ts while this
    // component was unmounted — just read it and ensure listeners are armed.
    if (claudeActive.value) {
      footerVisible.value = true
      store.armClaudeListeners(props.paneId)
    }
  } else {
    const savedCwd = store.consumeSavedCwd(props.paneId)
    const claudeRestore = store.consumeSavedClaudeRestore(props.paneId)

    // Pre-populate footer state BEFORE creating the session so the terminal
    // has its final height (with footer visible) when we measure rows/cols.
    if (savedCwd) {
      sessionCwd.value = savedCwd
      folderName.value = savedCwd.replace(/\\/g, '/').split('/').filter(Boolean).pop() ?? null
      const git = await invoke<{ is_repo: boolean; branch: string | null }>('get_session_git_info', { cwd: savedCwd }).catch(() => null)
      gitInfo.value = git
      if (claudeRestore?.wasOpen) {
        footerVisible.value = true
        // Set lifecycle early so claudeActive=true and the footer actually
        // renders before safeFit — otherwise the terminal is sized to the
        // full container, then the footer appears later causing a resize
        // that sends SIGWINCH and triggers Claude's ghost cursor artefact.
        store.updateClaudePaneState(props.paneId, { lifecycle: 'launching', confirmed: false })
      }
      // Let Vue re-render the footer, then refit terminal to the new smaller area
      await nextTick()
      await new Promise<void>(r => requestAnimationFrame(() => r()))
      safeFit()
    }

    // Determine shell: prefer saved shell, then default setting
    const savedShell = store.consumeSavedShell(props.paneId)
    const shellType = savedShell ?? (isWindows && devSettings.defaultShell === 'gitbash' ? 'gitbash' : 'powershell')
    const shellPath = (shellType === 'gitbash' && gitBashPath.value) ? gitBashPath.value : null
    currentShell.value = shellPath ? 'gitbash' : 'powershell'
    store.setTerminalShell(props.paneId, currentShell.value)

    sessionId = await invoke<string>('create_session', { cols: term.cols, rows: term.rows, cwd: savedCwd ?? null, shell: shellPath })
    store.setPtySession(props.paneId, sessionId)

    unlisten = await listen<string>(`pty-output-${sessionId}`, (event) => {
      term.write(event.payload)
    })

    if (claudeRestore && sessionId) {
      if (claudeRestore.sessionId) {
        // Resume conversation — pre-register expected session so JSONL watcher
        // adopts the resumed file into *this* pane.
        invoke('set_expected_claude_session', { sessionId, claudeSessionId: claudeRestore.sessionId }).catch(() => {})
        store.updateClaudePaneState(props.paneId, {
          lifecycle: 'launching', confirmed: false, sessionId: claudeRestore.sessionId,
        })
        store.armClaudeListeners(props.paneId)
        setTimeout(() => {
          invoke('write_to_session', { sessionId, data: `claude --resume ${claudeRestore.sessionId}\r` })
        }, 500)
      } else if (claudeRestore.wasOpen) {
        // Launch fresh — Claude was open but no conversation to resume
        store.updateClaudePaneState(props.paneId, {
          lifecycle: 'launching', confirmed: false,
        })
        store.armClaudeListeners(props.paneId)
        setTimeout(() => {
          invoke('write_to_session', { sessionId, data: 'claude\r' })
        }, 500)
      }
    }
  }

  // Clipboard and special key handling
  term.attachCustomKeyEventHandler((e) => {
    if (e.type !== 'keydown') return true
    // Ctrl+Shift+C or Ctrl+C with selection → copy
    if (e.ctrlKey && e.code === 'KeyC' && (e.shiftKey || term.hasSelection())) {
      if (term.hasSelection()) {
        clipboardWrite(term.getSelection())
        term.clearSelection()
      }
      return false
    }
    // Ctrl+Shift+V or Ctrl+V → paste
    if (e.ctrlKey && e.code === 'KeyV') {
      e.preventDefault()
      clipboardRead().then(text => {
        if (text && sessionId) invoke('write_to_session', { sessionId, data: text })
      })
      return false
    }
    // Ctrl+Enter → send newline (for Claude multi-line input)
    if (e.ctrlKey && e.code === 'Enter') {
      if (sessionId) invoke('write_to_session', { sessionId, data: '\n' })
      return false
    }
    return true
  })

  term.onData((data) => {
    if (sessionId) invoke('write_to_session', { sessionId, data })
  })

  resizeObserver = new ResizeObserver(scheduleFit)
  resizeObserver.observe(terminalEl.value!)

  // Initial focus is handled by App.vue polling for the textarea.

  // Subscribe to CWD changes (Claude lifecycle handled by persistent listeners in pane.ts)
  await subscribeToSession(sessionId!)

  // Focus this terminal if it's the focused pane — must happen after full setup
  if (isFocused.value) {
    await nextTick()
    term?.focus()
  }
})

onBeforeUnmount(() => {
  if (focusHandler) window.removeEventListener('arbiter:request-focus', focusHandler)
  if (workspaceActivatedHandler) window.removeEventListener('arbiter:workspace-activated', workspaceActivatedHandler)
  unsubscribeAll()
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
  <div class="terminal-pane" :class="{ focused: isFocused, compact }" :data-pane-id="paneId" @mousedown="store.setFocus(paneId)">
    <div v-if="!compact" class="pane-toolbar">
      <!-- Left: Process title from OSC 0 -->
      <span class="toolbar-process" v-if="terminalTitle">{{ terminalTitle }}</span>
      <span class="toolbar-process" v-else>&nbsp;</span>

      <!-- Center: Terminal name + edit -->
      <div class="toolbar-name" @mousedown.stop>
        <template v-if="isEditingName">
          <input
            ref="nameInputEl"
            v-model="editNameValue"
            class="name-input"
            @keydown.enter="commitName"
            @keydown.escape="cancelEditName"
            @blur="commitName"
          />
        </template>
        <template v-else>
          <span class="name-label">{{ terminalName }}</span>
          <button class="toolbar-btn edit-btn" title="Rename terminal" @click="startEditName">
            <MdiIcon :path="mdiPencilOutline" :size="11" />
          </button>
        </template>
      </div>

      <span class="toolbar-spacer" />

      <!-- Right: Claude buttons -->
      <template v-if="!claudeActive">
        <button class="toolbar-btn claude-btn" title="Launch claude" @click="launchClaude" @mousedown.stop>
          <ClaudeIcon :size="14" />
        </button>
        <button class="toolbar-btn claude-btn" title="claude --continue" @click="continueClaude" @mousedown.stop>
          <ClaudeIcon :size="14" />
          <MdiIcon :path="mdiChevronDoubleRight" :size="14" class="continue-icon" />
        </button>
        <button
          v-if="gitBashPath && shellIdle"
          class="toolbar-btn shell-btn"
          :title="currentShell === 'powershell' ? 'Switch to Git Bash' : 'Switch to PowerShell'"
          @click="switchShell"
          @mousedown.stop
        >
          <MdiIcon :path="currentShell === 'powershell' ? mdiBash : mdiPowershell" :size="14" />
        </button>
      </template>

      <!-- Right: Info button -->
      <button
        v-if="claudeActive"
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
    <div v-if="infoPanelOpen && claudeActive" class="info-panel">
      <div class="info-row">
        <span class="info-label">Session ID</span>
        <span class="info-value id-value">{{ claudeState.sessionId ?? '—' }}</span>
      </div>
      <div v-if="claudeState.model" class="info-row">
        <span class="info-label">Model</span>
        <span class="info-value">{{ modelLabel(claudeState.model) }}</span>
      </div>
      <div class="info-row">
        <span class="info-label">Tokens in</span>
        <span class="info-value">{{ claudeState.inputTokens.toLocaleString() }}</span>
      </div>
      <div class="info-row">
        <span class="info-label">Tokens out</span>
        <span class="info-value">{{ claudeState.outputTokens.toLocaleString() }}</span>
      </div>
      <div class="info-row">
        <span class="info-label">Cache write</span>
        <span class="info-value">{{ claudeState.cacheWriteTokens.toLocaleString() }}</span>
      </div>
      <div class="info-row">
        <span class="info-label">Cache read</span>
        <span class="info-value">{{ claudeState.cacheReadTokens.toLocaleString() }}</span>
      </div>
    </div>

    <div ref="terminalEl" class="terminal-inner" />
    <div v-if="claudeWorking" class="progress-bar">
      <div class="progress-bar-inner" />
    </div>
    <TerminalFooter
      v-if="inProjectWorkspace
        ? (claudeActive && footerVisible)
        : ((claudeActive && footerVisible) || gitInfo?.is_repo || (devSettings.alwaysShowFooter && sessionCwd))"
      :claude-running="claudeActive"
      :status="claudeActive ? {
        session_id: claudeState.sessionId ?? '',
        model_id: claudeState.model,
        input_tokens: claudeState.inputTokens,
        output_tokens: claudeState.outputTokens,
        cache_creation_input_tokens: claudeState.cacheWriteTokens,
        cache_read_input_tokens: claudeState.cacheReadTokens,
      } : null"
      :folder-name="folderName"
      :git-info="gitInfo"
      :session-id="sessionId"
      :hide-git-menu="inProjectWorkspace"
    />
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

.pane-toolbar {
  position: relative;
  display: flex;
  align-items: center;
  gap: 4px;
  height: 30px;
  padding: 0 6px;
  background: var(--color-bg-subtle);
  border-bottom: 1px solid var(--color-card-border);
  flex-shrink: 0;
  transition: background 0.12s;
}

.terminal-pane.focused .pane-toolbar {
  background: var(--color-bg-elevated);
}

.toolbar-spacer { flex: 1; }

.toolbar-process {
  font-size: 10px;
  color: var(--color-text-secondary);
  opacity: 0.8;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 200px;
  user-select: none;
  min-width: 0;
}

.toolbar-name {
  position: absolute;
  left: 50%;
  transform: translateX(-50%);
  display: flex;
  align-items: center;
  gap: 4px;
  min-width: 0;
}

.name-label {
  font-size: 11px;
  color: var(--color-text-muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 160px;
  user-select: none;
  transition: color 0.12s;
}

.terminal-pane.focused .name-label {
  color: var(--azure-tropical);
}

.name-input {
  font-size: 11px;
  font-family: inherit;
  color: var(--color-text-primary);
  background: var(--color-bg);
  border: 1px solid var(--color-accent);
  border-radius: 3px;
  padding: 1px 4px;
  outline: none;
  width: 120px;
}

.edit-btn {
  padding: 1px 3px;
  opacity: 0;
  transition: opacity 0.15s, color 0.15s, border-color 0.15s, background 0.15s;
}

.toolbar-name:hover .edit-btn,
.edit-btn:focus {
  opacity: 1;
}

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

.shell-btn:hover {
  color: var(--color-accent);
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

.progress-bar {
  position: absolute;
  top: 30px; /* right below the toolbar */
  left: 0;
  right: 0;
  height: 3px;
  overflow: hidden;
  z-index: 5;
  pointer-events: none;
}

.progress-bar-inner {
  height: 100%;
  width: 100%;
  background: linear-gradient(
    90deg,
    transparent 0%,
    var(--azure) 50%,
    transparent 100%
  );
  background-size: 50% 100%;
  background-repeat: no-repeat;
  animation: progress-slide 3s ease-in-out infinite alternate;
}

@keyframes progress-slide {
  0%   { background-position: -20% 0; }
  100% { background-position: 120% 0; }
}

.compact .progress-bar {
  top: 0;
}

.terminal-inner {
  flex: 1;
  min-height: 0;
  overflow: hidden;
  padding-left: 4px;
}



</style>
