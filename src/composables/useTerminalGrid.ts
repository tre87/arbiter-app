// Singleton manager for the GPU single-canvas terminal renderer (production).
//
// One transparent WebGL canvas floats over the whole window. The Rust backend
// (termgrid.rs) parses each attached PTY session with alacritty_terminal and
// streams binary cell-diffs over a Channel; here we decode them into per-pane
// cell grids and draw every visible pane into that ONE canvas at each pane's
// terminal-content rect. Empty/default-bg cells are skipped — the pane's own
// (opaque) background shows through the transparent canvas.
//
// Performance design:
//  - The cell size is taken from each pane's rect ÷ cols/rows, so the grid
//    EXACTLY fills the pane (no overflow/gap) and matches xterm's own sizing.
//  - Pane rects are CACHED and refreshed only on layout events (ResizeObserver,
//    window resize, workspace switch, a slow safety interval) — never per frame.
//    Reading getBoundingClientRect every frame forces a synchronous reflow that
//    interleaves with Vue's DOM writes and causes ~50ms jank.
//  - We only redraw when something changed (a diff, the cursor blink, or a
//    layout refresh), so idle costs nothing.

import { watch } from 'vue'
import { invoke, Channel } from '@tauri-apps/api/core'
import { SingleCanvasRenderer } from './singleCanvasRenderer'
import { usePerfStore } from '../stores/perf'
import { usePaneStore } from '../stores/pane'
import { pickPlatformTheme, CUSTOM_TERMINAL_BG } from '../themes/terminalThemes'

// Must match xterm's font (useXtermInstance) so glyph rasterisation matches.
const FONT_FAMILY = "Consolas, 'Cascadia Code', Menlo, 'SF Mono', monospace"
const FONT_SIZE = 12

const FLAG_INVERSE = 1 << 0
const FLAG_BOLD = 1 << 1
const FLAG_HIDDEN = 1 << 4
const FLAG_WIDE_SPACER = 1 << 6

// Default terminal bg — taken from the SAME xterm theme as the old renderer, so
// colors match exactly. Empty cells with this bg are skipped (the pane's own
// background shows through the transparent canvas). Set by getTheme().
let bgR = 0x12, bgG = 0x12, bgB = 0x12
// Selection highlight background (VS Code-ish blue).
const SEL_R = 0x26, SEL_G = 0x4f, SEL_B = 0x78
// Link foreground (GitHub link blue) so URLs stand out.
const LINK_R = 0x58, LINK_G = 0xa6, LINK_B = 0xff
// Search match backgrounds: all matches (dark gold) + current match (bright).
const MATCH_R = 0x5a, MATCH_G = 0x4a, MATCH_B = 0x12
const CUR_R = 0xc2, CUR_G = 0x8a, CUR_B = 0x24

function hexToRgb(hex: string): [number, number, number] {
  let h = (hex || '').replace('#', '').trim()
  if (h.length === 3) h = h.split('').map((c) => c + c).join('')
  const n = parseInt(h || '000000', 16)
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255]
}

interface ThemePayload { fg: [number, number, number]; bg: [number, number, number]; ansi: number[] }
let themePayload: ThemePayload | null = null

/** The xterm theme (pickPlatformTheme + custom-bg toggle) as a flat palette for
 *  the backend: default fg/bg + 16 ANSI colors. Computed once. */
function getTheme(): ThemePayload {
  if (themePayload) return themePayload
  const t = pickPlatformTheme()
  // GPU terminal background is always Arbiter's terminal color (== --color-bg),
  // matching xterm (forced via the `bg` option) and the pane — so the area is
  // this color from the first paint, never iTerm2 black.
  const bgHex = CUSTOM_TERMINAL_BG
  const fg = hexToRgb(t.foreground ?? '#ffffff')
  const bg = hexToRgb(bgHex)
  const names = [t.black, t.red, t.green, t.yellow, t.blue, t.magenta, t.cyan, t.white,
    t.brightBlack, t.brightRed, t.brightGreen, t.brightYellow, t.brightBlue, t.brightMagenta, t.brightCyan, t.brightWhite]
  const ansi: number[] = []
  for (const c of names) { const [r, g, b] = hexToRgb(c ?? '#ffffff'); ansi.push(r, g, b) }
  bgR = bg[0]; bgG = bg[1]; bgB = bg[2]
  themePayload = { fg, bg, ansi }
  return themePayload
}

/** The terminal background hex (for the pane's own div behind the canvas). */
export function terminalBgHex(): string {
  return CUSTOM_TERMINAL_BG
}

interface GridPane {
  slot: number
  paneId: string
  el: HTMLElement
  cols: number
  rows: number
  code: Uint32Array
  fg: Uint8Array
  bg: Uint8Array
  flags: Uint8Array
  link: Uint8Array // 1 where the cell is part of a detected URL
  cursorRow: number
  cursorCol: number
  cursorVisible: number
  // Scroll offset (lines into history); visible row r shows content line r-offset.
  offset: number
  // Cached layout (device px), refreshed only on layout events.
  visible: boolean
  rectLeft: number
  rectTop: number
  rectW: number
  rectH: number
}

let renderer: SingleCanvasRenderer | null = null
let canvasEl: HTMLCanvasElement | null = null
const bySlot = new Map<number, GridPane>()
const slotBySession = new Map<string, number>()
let nextSlot = 1
let raf = 0
let resizeObs: ResizeObserver | null = null
let drawScheduled = false
let perfTimer: ReturnType<typeof setInterval> | null = null
let stopFocusWatch: (() => void) | null = null
let perf: ReturnType<typeof usePerfStore> | null = null
let paneStore: ReturnType<typeof usePaneStore> | null = null
let loggedDecodeErr = false
let needsDraw = false

// Perf sampling (transport + decode + draw).
let windowStart = 0
let bytesAcc = 0
let framesRecv = 0
let decodeAcc = 0
let lastDrawMs = 0

// WebGL2 capability probe (memoized). When false, callers fall back to the
// per-terminal xterm renderer instead of the single-canvas GPU path — some
// WebView2 / VM / RDP environments lack WebGL2 and would otherwise render
// nothing. Gates both SharedTerminalCanvas (App.vue) and TerminalPane's `gpu`.
let gpuSupported: boolean | null = null
export function gpuRendererSupported(): boolean {
  if (gpuSupported === null) {
    try {
      gpuSupported = !!document.createElement('canvas').getContext('webgl2')
    } catch {
      gpuSupported = false
    }
  }
  return gpuSupported
}

export function initTerminalCanvas(canvas: HTMLCanvasElement) {
  canvasEl = canvas
  perf = usePerfStore()
  paneStore = usePaneStore()
  const dpr = window.devicePixelRatio || 1
  try {
    renderer = new SingleCanvasRenderer(canvas, {
      fontFamily: FONT_FAMILY, fontSize: FONT_SIZE, dpr, alpha: true, lineHeight: 1.0,
    })
  } catch (e) {
    // Defensive: gpuRendererSupported() gates this, but if construction fails
    // anyway (e.g. shader/context loss) leave the renderer null rather than
    // throwing out of onMounted. Terminals show their xterm layer / blank; the
    // user can disable the GPU renderer in Settings.
    console.error('Arbiter: GPU renderer init failed; disable the GPU renderer in Settings.', e)
    renderer = null
    return
  }
  resizeCanvas()
  window.addEventListener('resize', onWindowResize)
  // Workspace switch toggles display:none — defer a frame so the now-visible
  // panes have laid out before we read their rects.
  window.addEventListener('arbiter:workspace-activated', scheduleRefresh)
  // ResizeObserver fires AFTER layout, so reading rects in refreshRects from it
  // is free (no forced reflow). Covers split drags / sidebar toggles / resizes.
  // No polling timer: reading layout at an arbitrary tick forces a reflow that
  // scales with pane count and was the recurring jank.
  resizeObs = new ResizeObserver(refreshRects)
  for (const p of bySlot.values()) resizeObs.observe(p.el)

  const ch = new Channel<ArrayBuffer>()
  ch.onmessage = (msg) => decode(msg as unknown as ArrayBuffer)
  invoke('termgrid_start', { channel: ch }).catch((e) => console.error('Arbiter: termgrid_start failed', e))

  refreshRects()
  // Redraw on focus change so the (static) cursor moves to the newly focused
  // pane (the perpetual loop used to poll focusedId every frame for this).
  stopFocusWatch = watch(() => paneStore!.focusedId, markDirty)
  windowStart = performance.now()
  perfTimer = setInterval(samplePerf, 500)
  markDirty() // initial paint
  perf.setGpuActive(true)
}

export function teardownTerminalCanvas() {
  cancelAnimationFrame(raf)
  drawScheduled = false
  if (perfTimer) { clearInterval(perfTimer); perfTimer = null }
  stopFocusWatch?.(); stopFocusWatch = null
  resizeObs?.disconnect(); resizeObs = null
  window.removeEventListener('resize', onWindowResize)
  window.removeEventListener('arbiter:workspace-activated', scheduleRefresh)
  renderer = null
  canvasEl = null
  perf?.setGpuActive(false)
}

function resizeCanvas() {
  if (!renderer || !canvasEl) return
  const dpr = window.devicePixelRatio || 1
  const w = window.innerWidth
  const h = window.innerHeight
  renderer.resize(Math.floor(w * dpr), Math.floor(h * dpr))
  canvasEl.style.width = `${w}px`
  canvasEl.style.height = `${h}px`
}

// Only the window changing size needs the GL drawing buffer reallocated.
function onWindowResize() {
  resizeCanvas()
  refreshRects()
}
// Workspace switch fires before display:none toggles settle — wait one frame.
function scheduleRefresh() {
  requestAnimationFrame(refreshRects)
}

/** Re-read every pane's rect + visibility into the cache (the only place that
 *  touches the layout-forcing getBoundingClientRect / offsetParent). */
function refreshRects() {
  const dpr = window.devicePixelRatio || 1
  for (const pane of bySlot.values()) {
    const el = pane.el
    if (!el || el.offsetParent === null) { pane.visible = false; continue }
    const r = el.getBoundingClientRect()
    pane.visible = true
    pane.rectLeft = Math.round(r.left * dpr)
    pane.rectTop = Math.round(r.top * dpr)
    pane.rectW = Math.round(r.width * dpr)
    pane.rectH = Math.round(r.height * dpr)
  }
  markDirty()
}

export function attachPane(sessionId: string, paneId: string, el: HTMLElement, cols: number, rows: number, cellW?: number, cellH?: number) {
  let slot = slotBySession.get(sessionId)
  if (slot === undefined) {
    slot = nextSlot++
    slotBySession.set(sessionId, slot)
  }
  const len = Math.max(1, cols * rows)
  bySlot.set(slot, {
    slot, paneId, el, cols, rows,
    code: new Uint32Array(len), fg: new Uint8Array(len * 3),
    bg: new Uint8Array(len * 3), flags: new Uint8Array(len), link: new Uint8Array(len),
    cursorRow: 0, cursorCol: 0, cursorVisible: 0, offset: 0,
    visible: false, rectLeft: 0, rectTop: 0, rectW: 0, rectH: 0,
  })
  resizeObs?.observe(el)

  // Use xterm's EXACT measured device cell so the grid matches xterm's font
  // size + line height and fits the pane (rows×cellH ≤ pane height → no bottom
  // overflow). All panes share the font, so one global cell is correct.
  if (renderer && cellW && cellH && cellW >= 1 && cellH >= 1) {
    renderer.setCell(Math.round(cellW), Math.round(cellH))
  }

  const theme = getTheme()
  invoke('termgrid_attach', { sessionId, slot, cols, rows, fg: theme.fg, bg: theme.bg, ansi: theme.ansi })
    .catch((e) => console.error('Arbiter: termgrid_attach failed', e))
  refreshRects()
}

export function detachPane(sessionId: string) {
  const slot = slotBySession.get(sessionId)
  if (slot !== undefined) {
    const pane = bySlot.get(slot)
    if (pane) resizeObs?.unobserve(pane.el)
    bySlot.delete(slot)
    slotBySession.delete(sessionId)
    markDirty() // redraw without the removed pane
  }
  invoke('termgrid_detach', { sessionId }).catch(() => {})
}

// ── Selection (drag to select, copy text from the grid) ──────────────────────

// Selection is anchored to CONTENT lines (line = visibleRow - scrollOffset, so
// negative = scrollback), so it follows the buffer as it scrolls. Copy text is
// extracted in the backend (it has the full scrollback).
interface SelPoint { line: number; col: number }
let selSlot: number | null = null
let selAnchor: SelPoint | null = null
let selHead: SelPoint | null = null

function orderedSel(): { a: SelPoint; b: SelPoint } | null {
  if (selAnchor === null || selHead === null) return null
  let a = selAnchor, b = selHead
  if (a.line > b.line || (a.line === b.line && a.col > b.col)) { const t = a; a = b; b = t }
  return { a, b }
}

// Map a client (CSS px) point to a content cell {line, col}. Lines are clamped
// to [topVisible, bottomVisible] so dragging past the edge selects the edge row
// (the caller auto-scrolls to bring more into view).
function cellAt(slot: number, clientX: number, clientY: number): SelPoint | null {
  const r = renderer
  const pane = bySlot.get(slot)
  if (!r || !pane) return null
  const dpr = window.devicePixelRatio || 1
  const x = clientX * dpr - pane.rectLeft
  const y = clientY * dpr - pane.rectTop
  const col = Math.max(0, Math.min(pane.cols - 1, Math.floor(x / r.cellW)))
  const row = Math.max(0, Math.min(pane.rows - 1, Math.floor(y / r.cellH)))
  return { line: row - pane.offset, col }
}

export function selectionStart(sessionId: string, clientX: number, clientY: number) {
  const slot = slotBySession.get(sessionId)
  if (slot === undefined) return
  const p = cellAt(slot, clientX, clientY)
  if (!p) return
  selSlot = slot; selAnchor = p; selHead = p; markDirty()
}

export function selectionExtend(sessionId: string, clientX: number, clientY: number) {
  if (selSlot === null) return
  if (slotBySession.get(sessionId) !== selSlot) return
  const p = cellAt(selSlot, clientX, clientY)
  if (!p) return
  selHead = p; markDirty()
}

export function clearSelection() {
  if (selSlot !== null) { selSlot = null; selAnchor = null; selHead = null; markDirty() }
}

export function hasSelection(): boolean {
  return selSlot !== null && selAnchor !== null && selHead !== null &&
    !(selAnchor.line === selHead.line && selAnchor.col === selHead.col)
}

/** Ordered selection range in content-line coords, for backend text extraction. */
export function selectionRange(): { sLine: number; sCol: number; eLine: number; eCol: number } | null {
  const ord = orderedSel()
  if (!ord) return null
  return { sLine: ord.a.line, sCol: ord.a.col, eLine: ord.b.line, eCol: ord.b.col }
}

// ── Links ────────────────────────────────────────────────────────────────────

const URL_RE = /(https?:\/\/[^\s'"<>` ]+)/g

/** The http(s) URL under a client point, or null. Single-row detection. */
export function urlAt(sessionId: string, clientX: number, clientY: number): string | null {
  const slot = slotBySession.get(sessionId)
  const r = renderer
  if (slot === undefined || !r) return null
  const pane = bySlot.get(slot)
  if (!pane) return null
  const dpr = window.devicePixelRatio || 1
  const col = Math.floor((clientX * dpr - pane.rectLeft) / r.cellW)
  const row = Math.floor((clientY * dpr - pane.rectTop) / r.cellH)
  if (row < 0 || row >= pane.rows || col < 0 || col >= pane.cols) return null
  let text = ''
  for (let c = 0; c < pane.cols; c++) {
    const cp = pane.code[row * pane.cols + c]
    text += (cp === 0 || cp < 32 || (cp >= 0xd800 && cp <= 0xdfff) || cp > 0x10ffff) ? ' ' : String.fromCodePoint(cp)
  }
  URL_RE.lastIndex = 0
  let m: RegExpExecArray | null
  while ((m = URL_RE.exec(text)) !== null) {
    if (col >= m.index && col < m.index + m[0].length) {
      return m[0].replace(/[.,;:!?)\]}'"]+$/, '') // trim trailing punctuation
    }
  }
  return null
}

// Mark which cells of a row belong to a URL (so they render in link color).
// Called per dirty line on decode; cheap quick-reject for lines without ":" "/".
function markLinks(pane: GridPane, row: number) {
  const { cols, code, link } = pane
  const base = row * cols
  for (let c = 0; c < cols; c++) link[base + c] = 0
  let hasColon = false, hasSlash = false
  for (let c = 0; c < cols; c++) {
    const cp = code[base + c]
    if (cp === 58) hasColon = true
    else if (cp === 47) hasSlash = true
  }
  if (!hasColon || !hasSlash) return
  let text = ''
  for (let c = 0; c < cols; c++) {
    const cp = code[base + c]
    text += (cp === 0 || cp < 32 || (cp >= 0xd800 && cp <= 0xdfff) || cp > 0x10ffff) ? ' ' : String.fromCodePoint(cp)
  }
  URL_RE.lastIndex = 0
  let m: RegExpExecArray | null
  while ((m = URL_RE.exec(text)) !== null) {
    const end = m.index + m[0].replace(/[.,;:!?)\]}'"]+$/, '').length
    for (let c = m.index; c < end && c < cols; c++) link[base + c] = 1
  }
}

// ── Search ───────────────────────────────────────────────────────────────────

interface SearchMatch { line: number; col: number; len: number }
let searchSlot: number | null = null
let searchMatches: SearchMatch[] = []
let searchCurrent = -1

export function setSearch(sessionId: string, matches: SearchMatch[], current: number) {
  searchSlot = slotBySession.get(sessionId) ?? null
  searchMatches = matches
  searchCurrent = current
  markDirty()
}

export function clearSearch() {
  if (searchSlot !== null || searchMatches.length) {
    searchSlot = null; searchMatches = []; searchCurrent = -1; markDirty()
  }
}

export function searchMatchLine(index: number): number | null {
  const m = searchMatches[index]
  return m ? m.line : null
}

/** Scroll a content line into view (centered) if it's currently off-screen. */
export function scrollToLine(sessionId: string, line: number) {
  const slot = slotBySession.get(sessionId)
  if (slot === undefined) return
  const pane = bySlot.get(slot)
  if (!pane) return
  const vr = line + pane.offset // visible row if shown now
  if (vr >= 0 && vr < pane.rows) return // already visible
  const targetOffset = Math.max(0, Math.floor(pane.rows / 2) - line)
  const delta = targetOffset - pane.offset
  if (delta !== 0) invoke('termgrid_scroll', { sessionId, delta }).catch(() => {})
}

// ── Decode binary diffs ──────────────────────────────────────────────────────

function decode(msg: ArrayBuffer | ArrayBufferView | number[]) {
  let bytes: Uint8Array
  if (msg instanceof ArrayBuffer) bytes = new Uint8Array(msg)
  else if (ArrayBuffer.isView(msg)) bytes = new Uint8Array(msg.buffer, msg.byteOffset, msg.byteLength)
  else if (Array.isArray(msg)) bytes = Uint8Array.from(msg)
  else { if (!loggedDecodeErr) { loggedDecodeErr = true; console.error('Arbiter: unexpected termgrid payload', msg) } return }
  bytesAcc += bytes.byteLength
  framesRecv++
  const t0 = performance.now()
  try {
    decodeBody(bytes)
  } catch (e) {
    if (!loggedDecodeErr) { loggedDecodeErr = true; console.error('Arbiter: termgrid decode error', e) }
  }
  decodeAcc += performance.now() - t0
  markDirty()
}

function decodeBody(bytes: Uint8Array) {
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  let o = 0
  o += 1 // version
  const sc = dv.getUint16(o, true); o += 2
  for (let p = 0; p < sc; p++) {
    const slot = dv.getUint16(o, true); o += 2
    const cols = dv.getUint16(o, true); o += 2
    const rows = dv.getUint16(o, true); o += 2
    const curRow = dv.getUint16(o, true); o += 2
    const curCol = dv.getUint16(o, true); o += 2
    const curVis = bytes[o]; o += 1
    const offset = dv.getUint16(o, true); o += 2
    const dirtyLines = dv.getUint16(o, true); o += 2
    let pane = bySlot.get(slot)
    if (pane && (pane.cols !== cols || pane.rows !== rows)) {
      const len = Math.max(1, cols * rows)
      pane.cols = cols; pane.rows = rows
      pane.code = new Uint32Array(len)
      pane.fg = new Uint8Array(len * 3)
      pane.bg = new Uint8Array(len * 3)
      pane.flags = new Uint8Array(len)
      pane.link = new Uint8Array(len)
    }
    for (let dl = 0; dl < dirtyLines; dl++) {
      const row = dv.getUint16(o, true); o += 2
      const left = dv.getUint16(o, true); o += 2
      const right = dv.getUint16(o, true); o += 2
      for (let col = left; col <= right; col++) {
        const code = dv.getUint32(o, true); o += 4
        let fr = bytes[o], fg = bytes[o + 1], fb = bytes[o + 2]
        let br = bytes[o + 3], bgc = bytes[o + 4], bb = bytes[o + 5]
        const flags = bytes[o + 6]
        o += 7
        if (!pane || row >= pane.rows || col >= pane.cols) continue
        if (flags & FLAG_INVERSE) { const tr = fr, tg = fg, tb = fb; fr = br; fg = bgc; fb = bb; br = tr; bgc = tg; bb = tb }
        if (flags & FLAG_BOLD) { fr += ((255 - fr) * 0.25) | 0; fg += ((255 - fg) * 0.25) | 0; fb += ((255 - fb) * 0.25) | 0 }
        if (flags & FLAG_HIDDEN) { fr = br; fg = bgc; fb = bb }
        const ci = row * pane.cols + col
        pane.code[ci] = code
        pane.flags[ci] = flags
        const c3 = ci * 3
        pane.fg[c3] = fr; pane.fg[c3 + 1] = fg; pane.fg[c3 + 2] = fb
        pane.bg[c3] = br; pane.bg[c3 + 1] = bgc; pane.bg[c3 + 2] = bb
      }
      if (pane && row < pane.rows) markLinks(pane, row)
    }
    if (pane) { pane.cursorRow = curRow; pane.cursorCol = curCol; pane.cursorVisible = curVis; pane.offset = offset }
  }
}

// ── Draw all visible panes into the one canvas (only when something changed) ─

const uv = { u: 0, v: 0 }
// On-demand rendering: we schedule at most ONE rAF and only when something
// actually changed (a diff, selection, search, focus, or layout marked the
// canvas dirty). An idle app schedules nothing → ~0% CPU. The old perpetual
// rAF loop ran ~120×/sec doing a focus-poll + needsDraw check even when
// nothing changed; that was the entire idle CPU cost the profile flagged.
function scheduleDraw() {
  if (drawScheduled || !renderer) return
  drawScheduled = true
  raf = requestAnimationFrame(drawFrame)
}

function markDirty() {
  needsDraw = true
  scheduleDraw()
}

function drawFrame() {
  drawScheduled = false
  if (needsDraw) { drawAll(); needsDraw = false }
}

// Perf stats roll up on a cheap 500ms timer instead of every frame. Redundant
// store writes are skipped while persistently idle so nothing reactive churns.
let lastReportedFps = -1
function samplePerf() {
  if (!perf) return
  const now = performance.now()
  const secs = windowStart ? (now - windowStart) / 1000 : 0
  windowStart = now
  if (secs <= 0) return
  const fps = Math.round(framesRecv / secs)
  if (fps === 0 && lastReportedFps === 0) { framesRecv = 0; bytesAcc = 0; decodeAcc = 0; return }
  lastReportedFps = fps
  perf.setGpuStats({
    framesPerSec: fps,
    kbPerSec: Math.round(bytesAcc / 1024 / secs),
    decodeMs: Math.round((decodeAcc / Math.max(1, framesRecv)) * 100) / 100,
    drawMs: lastDrawMs,
  })
  framesRecv = 0; bytesAcc = 0; decodeAcc = 0
}

function drawAll() {
  const r = renderer
  if (!r) return
  const t0 = performance.now()
  let cap = bySlot.size
  for (const p of bySlot.values()) cap += p.cols * p.rows
  const data = r.ensureCapacity(cap)
  let n = 0
  const focusedPaneId = paneStore?.focusedId
  for (const pane of bySlot.values()) {
    if (!pane.visible) continue
    const ox = pane.rectLeft
    const oy = pane.rectTop
    const { cols, rows, code, fg, bg, flags, link } = pane
    const total = cols * rows
    const sel = (pane.slot === selSlot && hasSelection()) ? orderedSel() : null
    // Search highlight spans for this pane, keyed by visible row.
    let searchByRow: Map<number, Array<[number, number, boolean]>> | null = null
    if (pane.slot === searchSlot && searchMatches.length) {
      searchByRow = new Map()
      for (let i = 0; i < searchMatches.length; i++) {
        const sm = searchMatches[i]
        const vr = sm.line + pane.offset
        if (vr < 0 || vr >= rows) continue
        let arr = searchByRow.get(vr)
        if (!arr) { arr = []; searchByRow.set(vr, arr) }
        arr.push([sm.col, sm.col + sm.len, i === searchCurrent])
      }
    }
    for (let ci = 0; ci < total; ci++) {
      const fl = flags[ci]
      if (fl & FLAG_WIDE_SPACER) continue
      const col = ci % cols
      const row = (ci - col) / cols
      // Clip to the pane's actual rect — a very narrow/short pane can have more
      // cols/rows than fit (xterm's safeFit bails under 20 cols), so don't draw
      // cells that start outside the pane.
      if (col * r.cellW >= pane.rectW || row * r.cellH >= pane.rectH) continue
      let cp = code[ci]
      const c3 = ci * 3
      let br = bg[c3], bgc = bg[c3 + 1], bb = bg[c3 + 2]
      const absLine = row - pane.offset
      const selected = sel !== null &&
        (absLine > sel.a.line || (absLine === sel.a.line && col >= sel.a.col)) &&
        (absLine < sel.b.line || (absLine === sel.b.line && col <= sel.b.col))
      let searched = false, isCur = false
      if (!selected && searchByRow) {
        const spans = searchByRow.get(row)
        if (spans) for (const sp of spans) { if (col >= sp[0] && col < sp[1]) { searched = true; isCur = sp[2]; break } }
      }
      if (selected) {
        br = SEL_R; bgc = SEL_G; bb = SEL_B
        if (cp === 0) cp = 32 // draw the highlight even on empty cells
      } else if (searched) {
        br = isCur ? CUR_R : MATCH_R; bgc = isCur ? CUR_G : MATCH_G; bb = isCur ? CUR_B : MATCH_B
        if (cp === 0) cp = 32
      } else {
        // Never-written cells (code 0, bg 0,0,0 = black) and default-bg spaces
        // are skipped so the pane's own #121212 shows through (no black blocks).
        if (cp === 0) continue
        if (cp === 32 && br === bgR && bgc === bgG && bb === bgB) continue
      }
      if (cp < 32 || (cp >= 0xd800 && cp <= 0xdfff) || cp > 0x10ffff) cp = 32
      r.glyphUV(cp === 32 ? 32 : cp, uv)
      const o = n * 10
      data[o] = ox + col * r.cellW
      data[o + 1] = oy + row * r.cellH
      data[o + 2] = uv.u
      data[o + 3] = uv.v
      if (link[ci]) { data[o + 4] = LINK_R / 255; data[o + 5] = LINK_G / 255; data[o + 6] = LINK_B / 255 }
      else { data[o + 4] = fg[c3] / 255; data[o + 5] = fg[c3 + 1] / 255; data[o + 6] = fg[c3 + 2] / 255 }
      data[o + 7] = br / 255; data[o + 8] = bgc / 255; data[o + 9] = bb / 255
      n++
    }
    if (pane.paneId === focusedPaneId && pane.cursorVisible && pane.cursorCol < cols && pane.cursorRow < rows &&
        pane.cursorCol * r.cellW < pane.rectW && pane.cursorRow * r.cellH < pane.rectH) {
      const o = n * 10
      data[o] = ox + pane.cursorCol * r.cellW
      data[o + 1] = oy + pane.cursorRow * r.cellH
      data[o + 2] = 0; data[o + 3] = 0
      data[o + 4] = 0; data[o + 5] = 0; data[o + 6] = 0
      data[o + 7] = 0.8; data[o + 8] = 0.8; data[o + 9] = 0.85
      n++
    }
  }
  r.draw(data, n, [bgR / 255, bgG / 255, bgB / 255])
  lastDrawMs = Math.round((performance.now() - t0) * 100) / 100
}
