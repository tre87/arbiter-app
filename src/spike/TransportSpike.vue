<script setup lang="ts">
// SPIKE — Rust-parse + binary-diff transport renderer.
//
// The terminals are parsed in Rust (alacritty_terminal, one parser per pane on
// its own thread). Rust streams compact binary cell-diffs over a Tauri Channel;
// this component decodes them into per-pane cell grids and draws ALL panes into
// ONE WebGL2 canvas. Decisive test for: does moving VT parsing off the webview
// main thread + single-canvas GPU draw hold 60–120fps with many streams?
//
// Toggle with Ctrl/Cmd+Shift+G. Throwaway/measurement code.

import { onMounted, onBeforeUnmount, ref, shallowRef } from 'vue'
import { invoke, Channel } from '@tauri-apps/api/core'
import { SingleCanvasRenderer } from './singleCanvasRenderer'

const emit = defineEmits<{ (e: 'close'): void }>()

// Default terminal background (matches Rust HeadlessTerm.default_bg).
const DEFAULT_BG: [number, number, number] = [0x14 / 255, 0x14 / 255, 0x16 / 255]
const BG_R = 0x14, BG_G = 0x14, BG_B = 0x16
const FLAG_INVERSE = 1 << 0
const FLAG_HIDDEN = 1 << 4
const FLAG_WIDE_SPACER = 1 << 6

interface Pane {
  cols: number
  rows: number
  originX: number   // device px
  originY: number
  code: Uint32Array
  fg: Uint8Array    // cols*rows*3
  bg: Uint8Array
  flags: Uint8Array
  cursorRow: number
  cursorCol: number
  cursorVisible: number
}

const stageEl = ref<HTMLDivElement | null>(null)
const canvasEl = ref<HTMLCanvasElement | null>(null)

const paneCount = ref(20)
const stressing = ref(false)
const fps = ref(0)
const maxFrameGap = ref(0)
const framesPerSec = ref(0)
const kbPerSec = ref(0)
const decodeMs = ref(0)
const drawMs = ref(0)
const liveTerms = ref(0)

let renderer: SingleCanvasRenderer | null = null
const panes = shallowRef<Pane[]>([])
let raf = 0
let needsRender = true
let focusedIdx = 0
let cursorOn = true
let cursorTimer: ReturnType<typeof setInterval> | undefined

let deviceW = 0
let deviceH = 0

// ── Frame decode (binary → per-pane cell grids) ──────────────────────────────

let loggedDecodeErr = false
function decode(msg: ArrayBuffer | ArrayBufferView | number[]) {
  let bytes: Uint8Array
  if (msg instanceof ArrayBuffer) bytes = new Uint8Array(msg)
  else if (ArrayBuffer.isView(msg)) bytes = new Uint8Array(msg.buffer, msg.byteOffset, msg.byteLength)
  else if (Array.isArray(msg)) bytes = Uint8Array.from(msg)
  else { if (!loggedDecodeErr) { loggedDecodeErr = true; console.error('spike: unexpected channel payload', msg) } return }
  // Count "frames received" up front so the metric reflects TRANSPORT even if
  // the parse below hiccups — disambiguates a transport stall from a decode bug.
  bytesThisSec += bytes.byteLength
  framesThisSec++
  needsRender = true
  const t0 = performance.now()
  try {
    decodeBody(bytes)
  } catch (e) {
    if (!loggedDecodeErr) { loggedDecodeErr = true; console.error('spike: decode error', e) }
  }
  decodeAccum += performance.now() - t0
}

function decodeBody(bytes: Uint8Array) {
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  let o = 0
  o += 1 // version
  const pc = dv.getUint16(o, true); o += 2
  const list = panes.value
  for (let p = 0; p < pc; p++) {
    const idx = dv.getUint16(o, true); o += 2
    o += 4 // cols, rows (frontend already knows them; skip)
    const curRow = dv.getUint16(o, true); o += 2
    const curCol = dv.getUint16(o, true); o += 2
    const curVis = bytes[o]; o += 1
    const dirtyLines = dv.getUint16(o, true); o += 2
    const pane = list[idx]
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
        if (flags & FLAG_HIDDEN) { fr = br; fg = bgc; fb = bb }
        const ci = row * pane.cols + col
        pane.code[ci] = code
        pane.flags[ci] = flags
        const c3 = ci * 3
        pane.fg[c3] = fr; pane.fg[c3 + 1] = fg; pane.fg[c3 + 2] = fb
        pane.bg[c3] = br; pane.bg[c3 + 1] = bgc; pane.bg[c3 + 2] = bb
      }
    }
    if (pane) { pane.cursorRow = curRow; pane.cursorCol = curCol; pane.cursorVisible = curVis }
  }
}

// ── Render: build instances from all panes, one draw call ────────────────────

const uv = { u: 0, v: 0 }
function renderFrame() {
  const r = renderer
  if (!r) return
  const t0 = performance.now()
  const list = panes.value
  let cap = list.length // cursors
  for (const p of list) cap += p.cols * p.rows
  const data = r.ensureCapacity(cap)
  let n = 0
  for (const pane of list) {
    const { cols, rows, code, fg, bg, flags, originX, originY } = pane
    for (let ci = 0; ci < cols * rows; ci++) {
      const fl = flags[ci]
      if (fl & FLAG_WIDE_SPACER) continue
      let cp = code[ci]
      const c3 = ci * 3
      const br = bg[c3], bgc = bg[c3 + 1], bb = bg[c3 + 2]
      const isSpace = cp === 0 || cp === 32
      const defaultBg = br === BG_R && bgc === BG_G && bb === BG_B
      if (isSpace && defaultBg) continue
      if (cp < 32 || (cp >= 0xd800 && cp <= 0xdfff) || cp > 0x10ffff) cp = 32
      const col = ci % cols
      const row = (ci - col) / cols
      r.glyphUV(isSpace ? 32 : cp, uv)
      const o = n * 10
      data[o] = originX + col * r.cellW
      data[o + 1] = originY + row * r.cellH
      data[o + 2] = uv.u
      data[o + 3] = uv.v
      data[o + 4] = fg[c3] / 255; data[o + 5] = fg[c3 + 1] / 255; data[o + 6] = fg[c3 + 2] / 255
      data[o + 7] = br / 255; data[o + 8] = bgc / 255; data[o + 9] = bb / 255
      n++
    }
  }
  // Block cursor on the focused pane.
  const fp = list[focusedIdx]
  if (fp && cursorOn && fp.cursorVisible && fp.cursorCol < fp.cols && fp.cursorRow < fp.rows) {
    const o = n * 10
    data[o] = fp.originX + fp.cursorCol * r.cellW
    data[o + 1] = fp.originY + fp.cursorRow * r.cellH
    data[o + 2] = 0; data[o + 3] = 0
    data[o + 4] = 0; data[o + 5] = 0; data[o + 6] = 0
    data[o + 7] = 0.8; data[o + 8] = 0.8; data[o + 9] = 0.85
    n++
  }
  r.draw(data, n, DEFAULT_BG)
  drawMs.value = Math.round((performance.now() - t0) * 100) / 100
}

// ── rAF loop + perf counters ─────────────────────────────────────────────────

let frames = 0
let lastFrame = 0
let maxGap = 0
let windowStart = 0
let bytesThisSec = 0
let framesThisSec = 0
let decodeAccum = 0

function loop(t: number) {
  frames++
  const gap = t - lastFrame; if (lastFrame && gap > maxGap) maxGap = gap; lastFrame = t
  if (needsRender) { renderFrame(); needsRender = false }
  if (windowStart && t - windowStart >= 500) {
    const secs = (t - windowStart) / 1000
    fps.value = Math.round(frames / secs)
    maxFrameGap.value = Math.round(maxGap)
    framesPerSec.value = Math.round(framesThisSec / secs)
    kbPerSec.value = Math.round(bytesThisSec / 1024 / secs)
    decodeMs.value = Math.round((decodeAccum / Math.max(1, framesThisSec)) * 100) / 100
    frames = 0; maxGap = 0; bytesThisSec = 0; framesThisSec = 0; decodeAccum = 0
    windowStart = t
  } else if (!windowStart) {
    windowStart = t
  }
  raf = requestAnimationFrame(loop)
}

// ── Layout + lifecycle ───────────────────────────────────────────────────────

function allocPanes(count: number, cols: number, rows: number, gridCols: number, paneWd: number, paneHd: number): Pane[] {
  const out: Pane[] = []
  for (let i = 0; i < count; i++) {
    const gx = i % gridCols, gy = Math.floor(i / gridCols)
    const len = cols * rows
    out.push({
      cols, rows,
      originX: gx * paneWd + 4,
      originY: gy * paneHd + 4,
      code: new Uint32Array(len),
      fg: new Uint8Array(len * 3),
      bg: new Uint8Array(len * 3),
      flags: new Uint8Array(len),
      cursorRow: 0, cursorCol: 0, cursorVisible: 0,
    })
  }
  return out
}

async function start(count: number) {
  const r = renderer
  if (!r || !deviceW || !deviceH) return
  const gridCols = Math.ceil(Math.sqrt(count))
  const gridRows = Math.ceil(count / gridCols)
  const gapd = 8
  const paneWd = Math.floor(deviceW / gridCols)
  const paneHd = Math.floor(deviceH / gridRows)
  const cols = Math.max(10, Math.floor((paneWd - gapd) / r.cellW))
  const rows = Math.max(4, Math.floor((paneHd - gapd) / r.cellH))

  panes.value = allocPanes(count, cols, rows, gridCols, paneWd, paneHd)
  focusedIdx = 0
  liveTerms.value = count
  needsRender = true

  const ch = new Channel<ArrayBuffer>()
  ch.onmessage = (msg) => decode(msg as unknown as ArrayBuffer)
  await invoke('spike_start', { channel: ch, count, cols, rows, cwd: null })
}

function setCount(n: number) {
  paneCount.value = n
  stressing.value = false
  start(n)
}

function toggleStress() {
  stressing.value = !stressing.value
  invoke(stressing.value ? 'spike_stress' : 'spike_stress_stop').catch(() => {})
}

function onCanvasClick(e: MouseEvent) {
  const dpr = window.devicePixelRatio || 1
  const x = e.offsetX * dpr, y = e.offsetY * dpr
  const r = renderer
  if (!r) return
  const list = panes.value
  for (let i = 0; i < list.length; i++) {
    const p = list[i]
    if (x >= p.originX && x < p.originX + p.cols * r.cellW && y >= p.originY && y < p.originY + p.rows * r.cellH) {
      focusedIdx = i; needsRender = true; return
    }
  }
}

function onKeydown(e: KeyboardEvent) {
  // Let the app's global toggles through (Ctrl/Cmd+Shift+*).
  if ((e.ctrlKey || e.metaKey) && e.shiftKey) return
  if (!panes.value[focusedIdx]) return
  let data = ''
  if (e.key === 'Enter') data = '\r'
  else if (e.key === 'Backspace') data = '\x7f'
  else if (e.key === 'Tab') data = '\t'
  else if (e.key === 'Escape') data = '\x1b'
  else if (e.key === 'ArrowUp') data = '\x1b[A'
  else if (e.key === 'ArrowDown') data = '\x1b[B'
  else if (e.key === 'ArrowRight') data = '\x1b[C'
  else if (e.key === 'ArrowLeft') data = '\x1b[D'
  else if (e.ctrlKey && e.key.length === 1) {
    const code = e.key.toUpperCase().charCodeAt(0) - 64
    if (code > 0 && code < 32) data = String.fromCharCode(code)
  } else if (e.key.length === 1 && !e.metaKey) data = e.key
  else return
  e.preventDefault()
  e.stopPropagation()
  invoke('spike_write', { idx: focusedIdx, data }).catch(() => {})
}

onMounted(async () => {
  const canvas = canvasEl.value, stage = stageEl.value
  if (!canvas || !stage) return
  const dpr = window.devicePixelRatio || 1
  const rect = stage.getBoundingClientRect()
  deviceW = Math.floor(rect.width * dpr)
  deviceH = Math.floor(rect.height * dpr)
  renderer = new SingleCanvasRenderer(canvas, { fontFamily: 'Menlo, Consolas, monospace', fontSize: 12, dpr })
  renderer.resize(deviceW, deviceH)
  canvas.style.width = `${rect.width}px`
  canvas.style.height = `${rect.height}px`

  window.addEventListener('keydown', onKeydown, { capture: true })
  cursorTimer = setInterval(() => { cursorOn = !cursorOn; needsRender = true }, 530)
  await start(paneCount.value)
  raf = requestAnimationFrame(loop)
})

onBeforeUnmount(() => {
  cancelAnimationFrame(raf)
  if (cursorTimer) clearInterval(cursorTimer)
  window.removeEventListener('keydown', onKeydown, { capture: true })
  invoke('spike_stop').catch(() => {})
})
</script>

<template>
  <div class="spike-overlay">
    <div class="spike-bar">
      <span class="spike-title">Transport spike — Rust parse → binary diff → 1 canvas</span>
      <span class="spike-stat" :class="{ good: fps >= 110, ok: fps >= 55 && fps < 110, bad: fps < 55 }">{{ fps }} fps</span>
      <span class="spike-stat">gap {{ maxFrameGap }}ms</span>
      <span class="spike-stat">{{ framesPerSec }} frm/s</span>
      <span class="spike-stat">{{ kbPerSec }} KB/s</span>
      <span class="spike-stat">dec {{ decodeMs }}ms</span>
      <span class="spike-stat">draw {{ drawMs }}ms</span>
      <span class="spike-stat">{{ liveTerms }} terms</span>
      <span class="spike-spacer" />
      <button v-for="n in [5, 10, 20, 30, 50]" :key="n" class="spike-btn" :class="{ active: paneCount === n }" @click="setCount(n)">{{ n }}</button>
      <button class="spike-btn" :class="{ active: stressing }" @click="toggleStress">{{ stressing ? 'stop' : 'stream all' }}</button>
      <button class="spike-btn close" @click="emit('close')">✕</button>
    </div>
    <div ref="stageEl" class="spike-stage">
      <canvas ref="canvasEl" class="spike-canvas" @click="onCanvasClick" />
    </div>
  </div>
</template>

<style scoped>
.spike-overlay {
  position: fixed;
  inset: 0;
  z-index: 9999;
  background: #141416;
  display: flex;
  flex-direction: column;
}
.spike-bar {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 10px;
  background: #1c1c20;
  border-bottom: 1px solid #2a2a30;
  font: 11px/1.4 Menlo, Consolas, monospace;
  color: #aaa;
  flex: 0 0 auto;
}
.spike-title { color: #ddd; font-weight: 600; }
.spike-stat { color: #888; }
.spike-stat.good { color: #23d18b; }
.spike-stat.ok { color: #f5f543; }
.spike-stat.bad { color: #f14c4c; }
.spike-spacer { flex: 1; }
.spike-btn {
  background: #2a2a30;
  color: #ccc;
  border: 1px solid #3a3a42;
  border-radius: 4px;
  padding: 3px 8px;
  font: 11px Menlo, Consolas, monospace;
  cursor: pointer;
}
.spike-btn.active { background: #3b8eea; color: #fff; border-color: #3b8eea; }
.spike-btn.close { color: #f14c4c; }
.spike-stage { flex: 1 1 auto; overflow: hidden; }
.spike-canvas { display: block; }
</style>
