<script setup lang="ts">
import { ref, computed, watch, nextTick, onMounted, onBeforeUnmount } from 'vue'
import type { Terminal } from '@xterm/xterm'
import { readText as clipboardRead, writeText as clipboardWrite } from '@tauri-apps/plugin-clipboard-manager'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { usePaneStore } from '../stores/pane'
import { useDevSettingsStore } from '../stores/devSettings'
import MdiIcon from './MdiIcon.vue'
import ClaudeIcon from './ClaudeIcon.vue'
import { mdiInformationOutline, mdiChevronDoubleRight, mdiPencilOutline, mdiBash, mdiPowershell } from '@mdi/js'
import TerminalFooter from './TerminalFooter.vue'
import TerminalInfoPanel from './TerminalInfoPanel.vue'
import { createXtermInstance, type XtermInstance } from '../composables/useXtermInstance'

const props = withDefaults(defineProps<{ paneId: string; compact?: boolean }>(), { compact: false })
const store = usePaneStore()
const devSettings = useDevSettingsStore()
const terminalEl = ref<HTMLDivElement>()
const isFocused = computed(() => store.focusedId === props.paneId)
let xterm: XtermInstance | null = null
let term: Terminal
let unlisten: UnlistenFn | null = null
let sessionId: string | null = null
let resizeObserver: ResizeObserver | null = null
let fitTimer: ReturnType<typeof setTimeout> | null = null
let focusHandler: (() => void) | null = null
let workspaceActivatedHandler: (() => void) | null = null

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

// Project workspaces render git actions in the worktree sidebar, so the
// terminal footer should only appear when Claude is running.
const inProjectWorkspace = computed(() => store.isPaneInProjectWorkspace(props.paneId))

const claudeWorking = computed(() => claudeState.value.lifecycle === 'working')

const infoPanelOpen = ref(false)
function toggleInfoPanel() { infoPanelOpen.value = !infoPanelOpen.value }

const isWindows = navigator.platform.startsWith('Win')
const gitBashPath = ref<string | null>(null)
const shellIdle = ref(false)
const currentShell = ref<'powershell' | 'gitbash'>('powershell')

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

function scheduleFit() {
  if (fitTimer) clearTimeout(fitTimer)
  fitTimer = setTimeout(() => xterm?.safeFit(), 50)
}

watch(isFocused, (focused) => {
  if (focused) term?.focus()
})

// shellIdle is driven by the shell-activity-{sid} event from the backend.
// When Claude is running the shell-switch button is hidden regardless.
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

async function switchShell() {
  if (!sessionId) return
  const cwd = sessionCwd.value

  unsubscribeAll()
  await invoke('close_session', { sessionId })
  store.removePtySession(props.paneId)

  term.clear()
  term.reset()

  const newShell = currentShell.value === 'powershell' ? gitBashPath.value : null
  currentShell.value = newShell ? 'gitbash' : 'powershell'
  store.setTerminalShell(props.paneId, currentShell.value)

  sessionId = await invoke<string>('create_session', {
    cols: term.cols,
    rows: term.rows,
    cwd: cwd ?? null,
    shell: newShell,
  })
  store.setPtySession(props.paneId, sessionId)

  unlisten = await listen<string>(`pty-output-${sessionId}`, (event) => {
    term.write(event.payload)
  })
  await subscribeToSession(sessionId)

  shellIdle.value = false
  term.focus()
}

onMounted(async () => {
  xterm = createXtermInstance(terminalEl.value!)
  term = xterm.term

  // Store OSC 0 title for display in toolbar (Claude detection is handled
  // by persistent listeners in pane.ts via JSONL/process monitoring).
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

  focusHandler = () => { if (isFocused.value) term?.focus() }
  window.addEventListener('arbiter:request-focus', focusHandler)

  // Background workspaces render with `display: none` so ResizeObserver doesn't
  // fire while they're hidden — if the window resized in the interim we need a
  // one-shot refit on reveal. offsetParent check skips panes in other still-
  // hidden workspaces. safeFit no-ops if dimensions haven't changed.
  workspaceActivatedHandler = () => {
    if (terminalEl.value?.offsetParent !== null) scheduleFit()
  }
  window.addEventListener('arbiter:workspace-activated', workspaceActivatedHandler)

  await new Promise<void>(r => requestAnimationFrame(() => requestAnimationFrame(() => r())))

  xterm.loadWebgl()
  xterm.safeFit()

  term.textarea?.addEventListener('focus', () => store.setFocus(props.paneId))

  term.onResize(({ cols, rows }) => {
    store.markResize(props.paneId)
    if (sessionId) invoke('resize_session', { sessionId, cols, rows })
  })

  if (isWindows) {
    gitBashPath.value = await invoke<string | null>('check_git_bash')
  }

  // Reuse existing PTY session if the pane survived a split/remount; otherwise create fresh
  const existingSession = store.getPtySession(props.paneId)
  if (existingSession) {
    sessionId = existingSession
    currentShell.value = store.getTerminalShell(props.paneId)

    unlisten = await listen<string>(`pty-output-${sessionId}`, (event) => {
      term.write(event.payload)
    })

    // Backend skips the actual resize (and SIGWINCH) when dimensions are
    // unchanged — avoids Claude's Ink TUI redrawing on worktree switch.
    store.markResize(props.paneId)
    const resized = await invoke<boolean>('resize_session', { sessionId, cols: term.cols, rows: term.rows })
    if (!resized) {
      const replay = await invoke<string>('get_session_replay', { sessionId })
      if (replay) term.write(replay)
    }

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
        // Set lifecycle early so footer renders before safeFit — otherwise a
        // later resize sends SIGWINCH and triggers Claude's ghost cursor.
        store.updateClaudePaneState(props.paneId, { lifecycle: 'launching', confirmed: false })
      }
      await nextTick()
      await new Promise<void>(r => requestAnimationFrame(() => r()))
      xterm.safeFit()
    }

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
        store.updateClaudePaneState(props.paneId, { lifecycle: 'launching', confirmed: false })
        store.armClaudeListeners(props.paneId)
        setTimeout(() => {
          invoke('write_to_session', { sessionId, data: 'claude\r' })
        }, 500)
      }
    }
  }

  term.attachCustomKeyEventHandler((e) => {
    if (e.type !== 'keydown') return true
    if (e.ctrlKey && e.code === 'KeyC' && (e.shiftKey || term.hasSelection())) {
      if (term.hasSelection()) {
        clipboardWrite(term.getSelection())
        term.clearSelection()
      }
      return false
    }
    if (e.ctrlKey && e.code === 'KeyV') {
      e.preventDefault()
      clipboardRead().then(text => {
        if (text && sessionId) invoke('write_to_session', { sessionId, data: text })
      })
      return false
    }
    // Ctrl+Enter → newline (for Claude multi-line input)
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

  await subscribeToSession(sessionId!)

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
  xterm?.dispose()
})
</script>

<template>
  <div class="terminal-pane" :class="{ focused: isFocused, compact }" :data-pane-id="paneId" @mousedown="store.setFocus(paneId)">
    <div v-if="!compact" class="pane-toolbar">
      <span class="toolbar-process" v-if="terminalTitle">{{ terminalTitle }}</span>
      <span class="toolbar-process" v-else>&nbsp;</span>

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

    <TerminalInfoPanel
      v-if="infoPanelOpen && claudeActive"
      :session-id="claudeState.sessionId"
      :model="claudeState.model"
      :input-tokens="claudeState.inputTokens"
      :output-tokens="claudeState.outputTokens"
      :cache-write-tokens="claudeState.cacheWriteTokens"
      :cache-read-tokens="claudeState.cacheReadTokens"
    />

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

.progress-bar {
  position: absolute;
  top: 30px;
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
