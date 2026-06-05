<script setup lang="ts">
import { ref, computed, watch, nextTick, onMounted, onBeforeUnmount } from 'vue'
import type { Terminal } from '@xterm/xterm'
import { readText as clipboardRead, writeText as clipboardWrite } from '@tauri-apps/plugin-clipboard-manager'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { usePaneStore } from '../stores/pane'
import { useDevSettingsStore } from '../stores/devSettings'
import { useConfirm } from '../composables/useConfirm'
import { usePerfStore } from '../stores/perf'
import type { GitInfo } from '../types/pane'
import MdiIcon from './MdiIcon.vue'
import ClaudeIcon from './ClaudeIcon.vue'
import { mdiInformationOutline, mdiChevronDoubleRight, mdiPencilOutline, mdiBash, mdiPowershell } from '@mdi/js'
import TerminalFooter from './TerminalFooter.vue'
import TerminalInfoPanel from './TerminalInfoPanel.vue'
import { createXtermInstance, type XtermInstance } from '../composables/useXtermInstance'
import {
  getTerminalSession, setTerminalSession, disposeTerminalSession,
  type TerminalSession,
} from '../composables/terminalSessionCache'
import { gitBashPath, ensureGitBashProbed } from '../composables/gitBashPath'
import { waitForShellIdle } from '../utils/shellIdle'
import {
  attachPane as gpuAttachPane, detachPane as gpuDetachPane, terminalBgHex as gpuTerminalBgHex,
  selectionStart as gpuSelectionStart, selectionExtend as gpuSelectionExtend,
  clearSelection as gpuClearSelection, hasSelection as gpuHasSelection, selectionRange as gpuSelectionRange,
} from '../composables/useTerminalGrid'
import { CUSTOM_TERMINAL_BG } from '../themes/terminalThemes'

const props = withDefaults(defineProps<{ paneId: string; compact?: boolean }>(), { compact: false })
const store = usePaneStore()
const devSettings = useDevSettingsStore()
const perf = usePerfStore()
const { confirm } = useConfirm()
// GPU single-canvas renderer mode (read once at mount; the toggle reloads the
// app). In this mode xterm stays mounted as the invisible INPUT layer (no PTY
// output is written to it, so it never parses), and the shared WebGL canvas
// draws the grid that Rust parses. Removes both the xterm WebGL layer and the
// main-thread VT parse.
const gpu = devSettings.useGpuRenderer
const terminalEl = ref<HTMLDivElement>()
const isFocused = computed(() => store.focusedId === props.paneId)
let xterm: XtermInstance | null = null
let term: Terminal
let unlisten: UnlistenFn | null = null
let sessionId: string | null = null
let session: TerminalSession | null = null
let wrapperEl: HTMLDivElement | null = null
let resizeObserver: ResizeObserver | null = null
let fitTimer: ReturnType<typeof setTimeout> | null = null
let focusHandler: (() => void) | null = null
let workspaceActivatedHandler: (() => void) | null = null
let gitRefreshTimer: ReturnType<typeof setTimeout> | null = null
let gitFocusHandler: (() => void) | null = null
let unlistenGitChanged: (() => void) | null = null

// Three callers register byte-identical pty-output listeners; this helper
// keeps the write callback in one place. `term` is captured by reference, so
// it picks up reassignments (cached vs fresh-create mount paths).
//
// Flow control: xterm parses/renders on the main thread, so a firehose of
// output (a heavy Claude turn) can outrun it and jank the whole UI. We track
// bytes written but not yet processed (xterm's write callback acks each chunk)
// and pause the backend PTY reader above a high watermark, resuming below a low
// one — the standard xterm.js back-pressure pattern. State lives in this closure,
// which survives remounts (the cached reattach reuses the same listener).
const FLOW_HIGH = 1 << 17 // 128 KiB unprocessed → pause the PTY
const FLOW_LOW = 1 << 14  // 16 KiB → resume
function attachPtyOutput(sid: string): Promise<UnlistenFn> {
  let pending = 0
  let paused = false
  return listen<string>(`pty-output-${sid}`, (event) => {
    perf.markOutput(props.paneId)
    const len = event.payload.length
    pending += len
    if (!paused && pending > FLOW_HIGH) {
      paused = true
      invoke('pause_session', { sessionId: sid }).catch(() => {})
    }
    term.write(event.payload, () => {
      pending -= len
      if (paused && pending < FLOW_LOW) {
        paused = false
        invoke('resume_session', { sessionId: sid }).catch(() => {})
      }
    })
  })
}

// All Claude lifecycle state is centralized in the pane store (claudePaneStates).
// Persistent event listeners in pane.ts handle claude-started/status/exited/bell.
// TerminalPane only reads state reactively for rendering.
const claudeState = computed(() => store.getClaudePaneState(props.paneId))
const claudeActive = computed(() => store.isClaudeActive(props.paneId))
const footerVisible = ref(true)

const sessionCwd = ref<string | null>(null)
const folderName = ref<string | null>(null)
const gitInfo = ref<GitInfo | null>(null)
// Mirror git info into the store so the overview window can show the same
// compact git stats as the footer.
watch(gitInfo, (g) => store.setPaneGitInfo(props.paneId, g))
let unlistenCwd: (() => void) | null = null
let unlistenActivity: (() => void) | null = null

// Reactive state that must survive Vue remounts lives on the persistent
// TerminalSession in the cache. On a remount we adopt the cached refs directly
// so OSC 0 titles and the active shell never reset.
const cachedAtSetup = getTerminalSession(props.paneId)
const terminalTitle = cachedAtSetup?.title ?? ref('')

// Project workspaces render git actions in the worktree sidebar, so the
// terminal footer should only appear when Claude is running.
const inProjectWorkspace = computed(() => store.isPaneInProjectWorkspace(props.paneId))

const claudeWorking = computed(() => claudeState.value.lifecycle === 'working')

const infoPanelOpen = ref(false)
function toggleInfoPanel() { infoPanelOpen.value = !infoPanelOpen.value }

const isWindows = navigator.platform.startsWith('Win')
const shellIdle = ref(false)
const currentShell = cachedAtSetup?.shell ?? ref<'powershell' | 'gitbash'>('powershell')

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

// Footer folder-icon click: rename this terminal to the repo name (the git
// toplevel basename, e.g. "arbiter-app" — not the cwd subfolder), behind a
// confirm dialog. Only meaningful when the terminal sits inside a git repo.
async function renameToRepoName() {
  const cwd = sessionCwd.value
  if (!cwd || !gitInfo.value?.is_repo) return
  const root = await invoke<string | null>('git_repo_root', { path: cwd }).catch(() => null)
  if (!root) return
  const repoName = root.replace(/\\/g, '/').split('/').filter(Boolean).pop()
  if (!repoName) return
  const ok = await confirm({
    title: `Rename terminal to "${repoName}"?`,
    message: `Set this terminal's name from "${terminalName.value}" to the repository name "${repoName}".`,
    confirmText: 'Rename',
    cancelText: 'Cancel',
  })
  if (ok) store.setTerminalName(props.paneId, repoName)
}

function scheduleFit() {
  if (fitTimer) clearTimeout(fitTimer)
  fitTimer = setTimeout(() => { if (gpu) gpuFit(); else xterm?.safeFit() }, 50)
}

// Hold a WebGL context only while this pane is actually visible. Background
// workspaces stay mounted (v-show → display:none), so without this every
// terminal across every workspace would keep a GL context + glyph atlas alive —
// WebKit chokes on that many (idle FPS collapses). offsetParent is null when the
// pane (or an ancestor workspace) is display:none.
function applyWebglVisibility() {
  // GPU mode: never load xterm's WebGL — the shared canvas renders, and xterm
  // stays a DOM-rendered (empty, invisible) input layer with no GPU context.
  if (gpu) return
  if (!xterm) return
  if (terminalEl.value?.offsetParent != null) xterm.loadWebgl()
  else xterm.unloadWebgl()
}

// GPU mode: register this pane's terminal element with the shared canvas and
// tell the backend to start parsing this session into a grid. Idempotent — a
// remount (split / workspace switch) rebinds the new element and the backend
// replays a full frame. Hides xterm visually (input still works via its
// textarea underneath the pointer-events:none canvas).
function gpuCellPx(): { cw: number; ch: number } | null {
  const core = (term as any)?._core
  const cw = core?._renderService?.dimensions?.device?.cell?.width
  const ch = core?._renderService?.dimensions?.device?.cell?.height
  if (!cw || !ch) return null
  return { cw, ch }
}

// GPU-mode fit: derive cols/rows straight from the pane rect + xterm's measured
// device cell, bypassing xterm.safeFit's "≥20 cols" guard so narrow panes wrap
// at their true width (the guard protects xterm scrollback, which is irrelevant
// here — the grid lives in the backend). term.resize → resize_session → grid.
function gpuFit() {
  if (!gpu || !term || !terminalEl.value) return
  const cell = gpuCellPx()
  if (!cell) return
  const dpr = window.devicePixelRatio || 1
  const rect = terminalEl.value.getBoundingClientRect()
  if (!rect.width || !rect.height) return
  const cols = Math.max(1, Math.floor((rect.width * dpr) / cell.cw))
  const rows = Math.max(1, Math.floor((rect.height * dpr) / cell.ch))
  if (cols !== term.cols || rows !== term.rows) term.resize(cols, rows)
}

// Mouse-wheel scrollback in GPU mode: the shared canvas is pointer-events:none,
// so the wheel lands on the pane element; route it to the backend grid's scroll.
// Does NOT clear the selection — content-anchored selection follows the buffer,
// so you can wheel to extend a long selection past the visible area.
function onGpuWheel(e: WheelEvent) {
  if (!gpu || !sessionId) return
  e.preventDefault()
  const n = Math.max(1, Math.min(10, Math.round(Math.abs(e.deltaY) / 12)))
  // Wheel up (deltaY<0) scrolls into history (positive delta).
  invoke('termgrid_scroll', { sessionId, delta: e.deltaY < 0 ? n : -n }).catch(() => {})
}

// Drag-to-select in GPU mode, driven by our own handlers so the highlight is
// LIVE (xterm's buffer is empty, so it can't track a real selection). We do NOT
// stopPropagation, so xterm still focuses on mousedown (typing keeps working);
// `user-select: none` on the pane stops the browser's native drag-select.
let gpuDragging = false
let gpuAutoScroll: ReturnType<typeof setInterval> | undefined
let gpuMouse = { x: 0, y: 0 }
function gpuEdgeDelta(): number {
  const el = terminalEl.value
  if (!el) return 0
  const r = el.getBoundingClientRect()
  if (gpuMouse.y < r.top) return 3       // above top → scroll into history
  if (gpuMouse.y > r.bottom) return -3   // below bottom → scroll toward latest
  return 0
}
function stopGpuAutoScroll() {
  if (gpuAutoScroll) { clearInterval(gpuAutoScroll); gpuAutoScroll = undefined }
}
function onGpuMouseMove(e: MouseEvent) {
  if (!gpuDragging || !sessionId) return
  gpuMouse = { x: e.clientX, y: e.clientY }
  gpuSelectionExtend(sessionId, e.clientX, e.clientY)
  // Auto-scroll while dragging past the top/bottom edge, extending as it goes.
  if (gpuEdgeDelta() !== 0 && !gpuAutoScroll) {
    gpuAutoScroll = setInterval(() => {
      if (!sessionId) return
      const d = gpuEdgeDelta()
      if (d === 0) { stopGpuAutoScroll(); return }
      invoke('termgrid_scroll', { sessionId, delta: d }).catch(() => {})
      gpuSelectionExtend(sessionId, gpuMouse.x, gpuMouse.y)
    }, 50)
  } else if (gpuEdgeDelta() === 0) {
    stopGpuAutoScroll()
  }
}
function onGpuMouseUp() {
  gpuDragging = false
  stopGpuAutoScroll()
  window.removeEventListener('mousemove', onGpuMouseMove)
  window.removeEventListener('mouseup', onGpuMouseUp)
}
// Copy our grid selection on the native copy event (Cmd/Ctrl+C). preventDefault
// stops xterm copying its own (empty) selection; we fetch the text from the
// backend (it has the full scrollback) and write it asynchronously.
function onGpuCopy(e: ClipboardEvent) {
  if (!gpu || !sessionId || !gpuHasSelection()) return
  e.preventDefault()
  const r = gpuSelectionRange()
  if (!r) return
  invoke<string>('termgrid_selection_text', { sessionId, sLine: r.sLine, sCol: r.sCol, eLine: r.eLine, eCol: r.eCol })
    .then((text) => { if (text) clipboardWrite(text) })
    .catch(() => {})
}

function onGpuMouseDown(e: MouseEvent) {
  if (!gpu || !sessionId || e.button !== 0) return
  // Capture the drag for our own selection and keep xterm from starting its own
  // (empty) one. preventDefault cancels the browser's default focus change so
  // the explicit term.focus() below sticks — without it, typing breaks.
  e.preventDefault()
  e.stopPropagation()
  store.setFocus(props.paneId)
  term?.focus()
  gpuMouse = { x: e.clientX, y: e.clientY }
  gpuSelectionStart(sessionId, e.clientX, e.clientY)
  gpuDragging = true
  window.addEventListener('mousemove', onGpuMouseMove)
  window.addEventListener('mouseup', onGpuMouseUp)
}

function gpuAttach() {
  if (!gpu || !sessionId || !terminalEl.value) return
  // Hide xterm's own cursor — we draw ours on the shared canvas. Deliberately
  // NOT via opacity/transform/filter: those promote each terminal to its own
  // compositing layer (WebKit), reintroducing the exact per-frame cost the
  // single canvas removes. The empty xterm rows already match the terminal
  // background (#141416), so they're invisible without hiding the element.
  if (term) {
    term.options.theme = { ...(term.options.theme ?? {}), cursor: 'rgba(0,0,0,0)', cursorAccent: 'rgba(0,0,0,0)' }
  }
  // Paint the pane background to match the theme — the transparent canvas lets
  // empty cells show this through.
  terminalEl.value.style.background = gpuTerminalBgHex()
  // Size the grid to the true pane width (narrow panes included), then attach
  // with xterm's EXACT measured device cell so the grid fits the pane.
  gpuFit()
  const cell = gpuCellPx()
  gpuAttachPane(sessionId, props.paneId, terminalEl.value, term?.cols ?? 80, term?.rows ?? 24, cell?.cw, cell?.ch)
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
      .catch(e => console.error('Arbiter: claude launch failed:', e))
    term?.focus()
  }
}

function continueClaude() {
  if (sessionId) {
    store.updateClaudePaneState(props.paneId, { lifecycle: 'launching', confirmed: false, sessionId: null })
    store.armClaudeListeners(props.paneId)
    footerVisible.value = true
    invoke('write_to_session', { sessionId, data: 'claude --continue\r' })
      .catch(e => console.error('Arbiter: claude --continue failed:', e))
    term?.focus()
  }
}

// Claude lifecycle events (started/status/exited/bell/shell-activity) are
// handled by persistent listeners in pane.ts — they survive component
// unmount/remount. This function only subscribes to CWD changes.
async function subscribeToSession(sid: string) {
  unlistenCwd = await listen<{ cwd: string; folder: string | null; git: GitInfo }>(
    `cwd-changed-${sid}`,
    (event) => {
      sessionCwd.value = event.payload.cwd
      folderName.value = event.payload.folder
      gitInfo.value = event.payload.git
      updateGitWatch(sid, event.payload.cwd)
    },
  )

  // Backend fires this when the repo's .git dir changes (stage/commit/branch),
  // including ops made from VS Code. Refresh the footer counts (debounced).
  unlistenGitChanged = await listen(`git-changed-${sid}`, () => scheduleGitRefresh())

  // Refresh when the app window regains focus — catches external working-tree
  // edits (new/modified files) made while we were in another app.
  if (!gitFocusHandler) {
    gitFocusHandler = () => refreshGitInfo()
    window.addEventListener('focus', gitFocusHandler)
  }

  // Live driver for the shell-switch button (gitBashPath && shellIdle). The
  // status/Claude-lifecycle shell-activity listener in paneClaudeEvents.ts is
  // only armed when Claude launches and never touches this ref, so a plain
  // shell needs its own subscription. Without it, shellIdle relies solely on
  // the one-shot get_session_shell_idle read below — which runs before the
  // shell's first prompt (OSC 133;A) on a fresh PTY, latching false. (In dev,
  // HMR remounts re-run this read after the prompt, masking the bug; release
  // has no remount, so the Git Bash button never appears.)
  unlistenActivity = await listen<boolean>(`shell-activity-${sid}`, (event) => {
    if (isWindows && gitBashPath.value && !claudeActive.value) {
      shellIdle.value = event.payload
    }
  })

  // Recover CWD that may have been emitted before this listener was attached
  const currentCwd = await invoke<string | null>('get_session_cwd', { sessionId: sid }).catch(() => null)
  if (currentCwd && !sessionCwd.value) {
    sessionCwd.value = currentCwd
    folderName.value = currentCwd.replace(/\\/g, '/').split('/').filter(Boolean).pop() ?? null
    const git = await invoke<GitInfo>('get_session_git_info', { cwd: currentCwd }).catch(() => null)
    if (git) gitInfo.value = git
  }
  if (sessionCwd.value) updateGitWatch(sid, sessionCwd.value)
  if (isWindows && gitBashPath.value && !claudeActive.value) {
    const currentIdle = await invoke<boolean | null>('get_session_shell_idle', { sessionId: sid }).catch(() => null)
    if (currentIdle !== null && currentIdle !== undefined) {
      shellIdle.value = currentIdle
    }
  }
}

// Tears down component-local subscriptions only. The session-level PTY
// listener lives on the cached TerminalSession and is managed separately —
// kept alive across remounts, and only replaced/disposed by switchShell or
// disposeTerminalSession.
function unsubscribeComponentLocal() {
  unlistenCwd?.(); unlistenCwd = null
  unlistenActivity?.(); unlistenActivity = null
  unlistenGitChanged?.(); unlistenGitChanged = null
  if (gitRefreshTimer) { clearTimeout(gitRefreshTimer); gitRefreshTimer = null }
  if (gitFocusHandler) { window.removeEventListener('focus', gitFocusHandler); gitFocusHandler = null }
  if (sessionId) invoke('unwatch_git', { sessionId }).catch(() => {})
}

// Git status only changes via OSC 7 on a cwd change, so staging from outside
// the terminal (VS Code, or `git add` at the prompt) wouldn't update the
// footer. Instead a backend watcher on the repo's .git dir fires
// `git-changed-{sid}`, and we also refresh on window focus (catches external
// working-tree edits when switching back from VS Code). No polling.
async function refreshGitInfo() {
  const cwd = sessionCwd.value
  if (!cwd) return
  // force: bypass the backend's 1.5s cache — the index just changed.
  const git = await invoke<GitInfo>('get_session_git_info', { cwd, force: true }).catch(() => null)
  if (git) gitInfo.value = git
}

// Coalesce watcher bursts (staging writes index.lock then renames to index,
// emitting several events) into one refresh.
function scheduleGitRefresh() {
  if (gitRefreshTimer) clearTimeout(gitRefreshTimer)
  gitRefreshTimer = setTimeout(refreshGitInfo, 150)
}

// Point the backend .git watcher at the current cwd's repo. Idempotent within
// the same repo; the backend swaps/clears the watcher as the repo root changes.
function updateGitWatch(sid: string, cwd: string | null) {
  if (!cwd) return
  invoke('watch_git', { sessionId: sid, cwd }).catch(() => {})
}

async function switchShell() {
  if (!sessionId) return
  const cwd = sessionCwd.value
  const oldSessionId = sessionId

  unsubscribeComponentLocal()
  unlisten?.(); unlisten = null
  if (session) session.ptyUnlisten = null
  try {
    await invoke('close_session', { sessionId: oldSessionId })
  } catch (e) {
    console.error('Arbiter: close_session failed during switchShell:', e)
  }
  store.removePtySession(props.paneId)

  term.clear()
  term.reset()

  const newShell = currentShell.value === 'powershell' ? gitBashPath.value : null
  currentShell.value = newShell ? 'gitbash' : 'powershell'
  store.setTerminalShell(props.paneId, currentShell.value)

  let newSessionId: string
  try {
    newSessionId = await invoke<string>('create_session', {
      cols: term.cols,
      rows: term.rows,
      cwd: cwd ?? null,
      shell: newShell,
    })
  } catch (e) {
    // create_session failed: leave the terminal visibly idle rather than
    // racing back to whatever cached sessionId was lingering on `session`.
    console.error('Arbiter: create_session failed during switchShell, terminal will be inactive:', e)
    sessionId = null
    if (session) session.sessionId = ''
    term.write('\r\n\x1b[31m[Arbiter] Shell switch failed — close and reopen this terminal.\x1b[0m\r\n')
    return
  }
  sessionId = newSessionId
  store.setPtySession(props.paneId, newSessionId)
  if (session) session.sessionId = newSessionId

  unlisten = await attachPtyOutput(newSessionId)
  if (session) session.ptyUnlisten = unlisten
  await subscribeToSession(newSessionId)

  shellIdle.value = false
  term.focus()
}

onMounted(async () => {
  const cached = getTerminalSession(props.paneId)

  if (cached) {
    // ── Reattach path: xterm + PTY listener survived the unmount ─────────
    // VS Code-style: the Terminal instance, its scrollback, and its event
    // handlers are preserved. We just reparent the wrapper element into the
    // new TerminalPane's container. No replay, no handler re-registration.
    session = cached
    xterm = cached.xterm
    term = xterm.term
    sessionId = cached.sessionId
    unlisten = cached.ptyUnlisten
    wrapperEl = cached.wrapperEl

    terminalEl.value!.appendChild(cached.wrapperEl)

    await new Promise<void>(r => requestAnimationFrame(() => requestAnimationFrame(() => r())))
    applyWebglVisibility()
    // Don't call safeFit here: the WebGL addon hasn't rendered a frame yet, so
    // _renderService.dimensions is stale. A wrong cell-width measurement would
    // resize xterm (and SIGWINCH the PTY) to narrow cols, baking Claude's
    // narrow wrap into scrollback forever. The Terminal's existing cols/rows
    // survived the detach; the ResizeObserver attached below will fire an
    // initial callback and refit via scheduleFit once dimensions stabilize.
    term.refresh(0, term.rows - 1)

    if (isWindows) await ensureGitBashProbed()

    if (claudeActive.value) footerVisible.value = true
  } else {
    // ── Fresh-create path ────────────────────────────────────────────────
    wrapperEl = document.createElement('div')
    wrapperEl.style.width = '100%'
    wrapperEl.style.height = '100%'
    terminalEl.value!.appendChild(wrapperEl)

    xterm = createXtermInstance(wrapperEl, gpu ? { bg: CUSTOM_TERMINAL_BG } : {})
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

    await new Promise<void>(r => requestAnimationFrame(() => requestAnimationFrame(() => r())))

    applyWebglVisibility()
    xterm.safeFit()

    term.textarea?.addEventListener('focus', () => store.setFocus(props.paneId))
    if (gpu) term.textarea?.addEventListener('copy', onGpuCopy)

    // Persistent handlers read the current session id through the cache so
    // shell-switch updates reach them even though they were registered once.
    term.onResize(({ cols, rows }) => {
      store.markResize(props.paneId)
      const sid = getTerminalSession(props.paneId)?.sessionId
      if (sid) {
        // A silent resize failure desyncs xterm and the PTY for the rest of
        // the session lifetime — Claude's TUI then misrenders forever.
        invoke('resize_session', { sessionId: sid, cols, rows })
          .catch(e => console.error('Arbiter: resize_session failed:', e))
      }
    })

    if (isWindows) await ensureGitBashProbed()

    // Fallback: a PTY is registered in the store but no xterm cache entry
    // exists. In normal flow the cache and store are populated together, so
    // this only fires if a previous fresh-create threw before `setTerminalSession`.
    // Rather than lose the live PTY, adopt it and replay its buffer.
    const existingSession = store.getPtySession(props.paneId)
    if (existingSession) {
      sessionId = existingSession
      currentShell.value = store.getTerminalShell(props.paneId)

      if (!gpu) unlisten = await attachPtyOutput(sessionId)

      // Backend skips the actual resize (and SIGWINCH) when dimensions are
      // unchanged — avoids Claude's Ink TUI redrawing on worktree switch.
      store.markResize(props.paneId)
      try {
        const resized = await invoke<boolean>('resize_session', { sessionId, cols: term.cols, rows: term.rows })
        if (!gpu && !resized) {
          const replay = await invoke<string>('get_session_replay', { sessionId })
          if (replay) term.write(replay)
        }
      } catch (e) {
        console.error('Arbiter: resize/replay failed on adoption fallback:', e)
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
        const git = await invoke<GitInfo>('get_session_git_info', { cwd: savedCwd }).catch(() => null)
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

      try {
        sessionId = await invoke<string>('create_session', { cols: term.cols, rows: term.rows, cwd: savedCwd ?? null, shell: shellPath })
      } catch (e) {
        // Fresh-create failed — surface in the terminal pane rather than
        // leaving a zombie xterm wired to a non-existent PTY.
        console.error('Arbiter: create_session failed during mount:', e)
        term.write('\r\n\x1b[31m[Arbiter] Failed to start shell. Close and reopen this terminal.\x1b[0m\r\n')
        return
      }
      store.setPtySession(props.paneId, sessionId)
      const newSessionId = sessionId

      if (!gpu) unlisten = await attachPtyOutput(newSessionId)

      if (claudeRestore) {
        if (claudeRestore.sessionId) {
          // Resume conversation — pre-register expected session so JSONL watcher
          // adopts the resumed file into *this* pane. Failure here lands the
          // resumed Claude output in some other waiting pane, so surface it.
          invoke('set_expected_claude_session', { sessionId: newSessionId, claudeSessionId: claudeRestore.sessionId })
            .catch(e => console.error('Arbiter: set_expected_claude_session failed — resume may adopt into the wrong pane:', e))
          store.updateClaudePaneState(props.paneId, {
            lifecycle: 'launching', confirmed: false, sessionId: claudeRestore.sessionId,
          })
          store.armClaudeListeners(props.paneId)
          waitForShellIdle(newSessionId).then(() => {
            invoke('write_to_session', { sessionId: newSessionId, data: `claude --resume ${claudeRestore.sessionId}\r` })
              .catch(e => console.error('Arbiter: claude --resume write failed:', e))
          })
        } else if (claudeRestore.wasOpen) {
          store.updateClaudePaneState(props.paneId, { lifecycle: 'launching', confirmed: false })
          store.armClaudeListeners(props.paneId)
          waitForShellIdle(newSessionId).then(() => {
            invoke('write_to_session', { sessionId: newSessionId, data: 'claude\r' })
              .catch(e => console.error('Arbiter: claude launch write failed:', e))
          })
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
          const sid = getTerminalSession(props.paneId)?.sessionId
          if (text && sid) invoke('write_to_session', { sessionId: sid, data: text })
        })
        return false
      }
      // Shift+Enter: when Claude is running in this pane, send the kitty
      // keyboard protocol sequence for Shift+Return (CSI 13;2 u) — Ink's
      // input parser (used by Claude Code) recognises it as a true
      // shift+enter and inserts a newline in the prompt, matching iTerm2's
      // behaviour. In a plain shell we send \r so Shift+Enter just behaves
      // like Enter; sending the kitty bytes there would risk literal output.
      if (e.shiftKey && !e.ctrlKey && !e.metaKey && !e.altKey && e.code === 'Enter') {
        e.preventDefault()
        const sid = getTerminalSession(props.paneId)?.sessionId
        if (sid) {
          const data = claudeState.value.lifecycle !== 'closed' ? '\x1b[13;2u' : '\r'
          invoke('write_to_session', { sessionId: sid, data })
        }
        return false
      }
      // Ctrl+Enter → \n (existing convention that already works in Claude
      // multi-line input on both platforms; leaving untouched).
      if (e.ctrlKey && e.code === 'Enter') {
        e.preventDefault()
        const sid = getTerminalSession(props.paneId)?.sessionId
        if (sid) invoke('write_to_session', { sessionId: sid, data: '\n' })
        return false
      }
      return true
    })

    term.onData((data) => {
      const sid = getTerminalSession(props.paneId)?.sessionId
      if (sid) {
        perf.markInput(props.paneId)
        const t0 = performance.now()
        invoke('write_to_session', { sessionId: sid, data })
          .then(() => perf.recordWrite(props.paneId, performance.now() - t0))
          .catch(() => {})
        // Typing returns the GPU viewport to the bottom (so you see your input)
        // and clears any selection.
        if (gpu) {
          gpuClearSelection()
          invoke('termgrid_scroll', { sessionId: sid, delta: -1_000_000 }).catch(() => {})
        }
      }
    })

    session = {
      xterm,
      wrapperEl,
      sessionId,
      ptyUnlisten: unlisten,
      title: terminalTitle,
      shell: currentShell,
    }
    setTerminalSession(props.paneId, session)
  }

  // ── Component-local setup (recreated every mount) ───────────────────────
  focusHandler = () => { if (isFocused.value) term?.focus() }
  window.addEventListener('arbiter:request-focus', focusHandler)

  // Background workspaces render with `display: none` so ResizeObserver doesn't
  // fire while they're hidden — if the window resized in the interim we need a
  // one-shot refit on reveal. offsetParent check skips panes in other still-
  // hidden workspaces. safeFit no-ops if dimensions haven't changed.
  workspaceActivatedHandler = () => {
    // Switching workspaces toggles display:none — load WebGL for the now-visible
    // pane and free it for hidden ones, capping live GL contexts to the active
    // workspace's terminals.
    applyWebglVisibility()
    if (terminalEl.value?.offsetParent !== null) scheduleFit()
  }
  window.addEventListener('arbiter:workspace-activated', workspaceActivatedHandler)

  resizeObserver = new ResizeObserver(scheduleFit)
  resizeObserver.observe(terminalEl.value!)

  if (sessionId) await subscribeToSession(sessionId)

  if (gpu) gpuAttach()

  if (isFocused.value) {
    await nextTick()
    term?.focus()
  }
})

onBeforeUnmount(() => {
  if (gpuDragging) onGpuMouseUp()
  if (focusHandler) window.removeEventListener('arbiter:request-focus', focusHandler)
  if (workspaceActivatedHandler) window.removeEventListener('arbiter:workspace-activated', workspaceActivatedHandler)
  if (fitTimer) clearTimeout(fitTimer)
  resizeObserver?.disconnect()
  unsubscribeComponentLocal()

  if (!store.hasPaneId(props.paneId)) {
    // Pane truly removed from layout — tear down everything.
    if (gpu && sessionId) gpuDetachPane(sessionId)
    disposeTerminalSession(props.paneId)
    if (sessionId) {
      invoke('close_session', { sessionId })
        .catch(e => console.error('Arbiter: close_session failed:', e))
      store.removePtySession(props.paneId)
    }
  } else {
    // Pane survives (split, worktree switch, workspace switch). Detach the
    // wrapper from the current DOM container; xterm + PTY listener stay
    // alive in the cache for the next mount to adopt. Free the WebGL context
    // while hidden so visible terminals always keep one (WebKit evicts the
    // oldest past its cap, silently dropping a visible pane to the DOM
    // renderer); loadWebgl() on the next mount re-acquires it.
    xterm?.unloadWebgl()
    wrapperEl?.remove()
  }
})
</script>

<template>
  <div class="terminal-pane" :class="{ focused: isFocused, compact, gpu }" :data-pane-id="paneId" @mousedown="store.setFocus(paneId)">
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
        <template v-if="!devSettings.hideClaudeButtons">
          <button class="toolbar-btn claude-btn" title="Launch claude" @click="launchClaude" @mousedown.stop>
            <ClaudeIcon :size="14" />
          </button>
          <button class="toolbar-btn claude-btn" title="claude --continue" @click="continueClaude" @mousedown.stop>
            <ClaudeIcon :size="14" />
            <MdiIcon :path="mdiChevronDoubleRight" :size="14" class="continue-icon" />
          </button>
        </template>
        <button
          v-if="gitBashPath && shellIdle && !devSettings.hideShellButton"
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

    <div ref="terminalEl" class="terminal-inner" @wheel="onGpuWheel" @mousedown.capture="onGpuMouseDown" />
    <!-- Mounted for the whole Claude session so the slide animation is never
         re-created mid-turn; the `working` class fades it in and resumes the
         (paused) animation from its current position — no reset across the
         working↔ready flicker between tool calls. -->
    <div v-if="claudeActive" class="progress-bar" :class="{ working: claudeWorking }">
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
        context_window_size: claudeState.contextWindowSize,
        used_percentage: claudeState.usedPercentage,
        has_context: claudeState.hasContext,
      } : null"
      :folder-name="folderName"
      :git-info="gitInfo"
      :session-id="sessionId"
      :hide-git-menu="inProjectWorkspace"
      @rename-to-repo="renameToRepoName"
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
  height: 34px;
  padding: 0 6px;
  background: var(--color-bg-titlebar);
  border-bottom: 1px solid var(--color-card-border);
  flex-shrink: 0;
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
  color: var(--color-text-secondary);
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
  width: 50%;
  background: linear-gradient(
    90deg,
    transparent 0%,
    var(--azure) 50%,
    transparent 100%
  );
  animation: progress-slide 3s ease-in-out infinite alternate;
  /* Hidden + paused while not actively working; the `working` class fades it in
     and resumes the animation from where it paused (no restart). transform +
     opacity keep this on the compositor — no per-frame repaint (the old
     background-position animation painted the bar every frame). */
  opacity: 0;
  animation-play-state: paused;
  transition: opacity 0.25s ease;
  will-change: transform;
}

.progress-bar.working .progress-bar-inner {
  opacity: 1;
  animation-play-state: running;
}

@keyframes progress-slide {
  0%   { transform: translateX(-100%); }
  100% { transform: translateX(300%); }
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

/* GPU mode: the shared canvas draws the grid on top, so drop the left padding
   (the grid origin = this element's rect). Default background avoids a launch
   flash before gpuAttach sets the exact theme background inline; empty cells
   show this through the transparent canvas. */
.terminal-pane.gpu .terminal-inner {
  padding-left: 0;
  background: #121212;
  /* We draw our own selection on the canvas; suppress the browser's native
     drag-select of the (empty) xterm DOM so it doesn't fight ours. */
  user-select: none;
}

/* Pin the whole pane to the terminal background in GPU mode so the area is the
   right color from the first paint — no black flash during startup before the
   canvas/first frame render. gpuAttach sets the exact theme bg inline. */
.terminal-pane.gpu {
  background: #121212;
}
</style>
