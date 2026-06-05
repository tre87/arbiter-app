// SPIKE — single-canvas GPU terminal renderer.
//
// Renders many terminal grids into ONE WebGL2 canvas (one compositing layer) to
// test whether that removes the per-canvas compositing ceiling that tanks FPS
// with many xterm WebGL instances. Throwaway/measurement code: text + fg/bg +
// cursor only — no selection/links/search/ligatures.
//
// Design: an on-demand glyph atlas (2D-rasterised glyphs uploaded to a texture)
// + one instanced draw call per frame. Each instance is one cell:
//   pos(2) atlasUV(2) fg(3) bg(3)  →  shader does mix(bg, fg, glyphAlpha).

const ATLAS_SIZE = 1024

const VERT = `#version 300 es
precision highp float;
layout(location=0) in vec2 aCorner;   // unit quad 0..1
layout(location=1) in vec2 aPos;      // cell top-left, device px
layout(location=2) in vec2 aUV;       // glyph atlas top-left, normalised
layout(location=3) in vec3 aFg;
layout(location=4) in vec3 aBg;
uniform vec2 uCanvas;   // device px
uniform vec2 uCell;     // device px
uniform vec2 uGlyph;    // glyph size in atlas, normalised
out vec2 vUV;
out vec3 vFg;
out vec3 vBg;
void main() {
  vec2 px = aPos + aCorner * uCell;
  vec2 clip = vec2((px.x / uCanvas.x) * 2.0 - 1.0, 1.0 - (px.y / uCanvas.y) * 2.0);
  gl_Position = vec4(clip, 0.0, 1.0);
  vUV = aUV + aCorner * uGlyph;
  vFg = aFg;
  vBg = aBg;
}`

const FRAG = `#version 300 es
precision highp float;
in vec2 vUV;
in vec3 vFg;
in vec3 vBg;
uniform sampler2D uAtlas;
out vec4 outColor;
void main() {
  float a = texture(uAtlas, vUV).a;
  outColor = vec4(mix(vBg, vFg, a), 1.0);
}`

// Block Elements (U+2580–U+259F): draw as exact filled rectangles so they tile
// seamlessly (matching xterm's customGlyphs), instead of relying on the font —
// which leaves gaps and breaks block art like the Claude avatar. Returns true
// if `cp` was handled. Coordinates are rounded so adjacent halves don't seam.
function drawBlockGlyph(ctx: CanvasRenderingContext2D, cp: number, x: number, y: number, w: number, h: number): boolean {
  if (cp < 0x2580 || cp > 0x259f) return false
  const R = Math.round
  const hx = R(w / 2), hy = R(h / 2)
  ctx.save()
  ctx.fillStyle = '#fff'
  switch (cp) {
    case 0x2588: ctx.fillRect(x, y, w, h); break                              // █ full
    case 0x2580: ctx.fillRect(x, y, w, hy); break                            // ▀ upper half
    case 0x2584: ctx.fillRect(x, y + hy, w, h - hy); break                   // ▄ lower half
    case 0x258c: ctx.fillRect(x, y, hx, h); break                            // ▌ left half
    case 0x2590: ctx.fillRect(x + hx, y, w - hx, h); break                   // ▐ right half
    case 0x2581: ctx.fillRect(x, y + R(h * 7 / 8), w, h - R(h * 7 / 8)); break // ▁
    case 0x2582: ctx.fillRect(x, y + R(h * 6 / 8), w, h - R(h * 6 / 8)); break
    case 0x2583: ctx.fillRect(x, y + R(h * 5 / 8), w, h - R(h * 5 / 8)); break
    case 0x2585: ctx.fillRect(x, y + R(h * 3 / 8), w, h - R(h * 3 / 8)); break
    case 0x2586: ctx.fillRect(x, y + R(h * 2 / 8), w, h - R(h * 2 / 8)); break
    case 0x2587: ctx.fillRect(x, y + R(h / 8), w, h - R(h / 8)); break        // ▇
    case 0x2589: ctx.fillRect(x, y, R(w * 7 / 8), h); break                  // ▉
    case 0x258a: ctx.fillRect(x, y, R(w * 6 / 8), h); break
    case 0x258b: ctx.fillRect(x, y, R(w * 5 / 8), h); break
    case 0x258d: ctx.fillRect(x, y, R(w * 3 / 8), h); break
    case 0x258e: ctx.fillRect(x, y, R(w * 2 / 8), h); break
    case 0x258f: ctx.fillRect(x, y, R(w / 8), h); break                       // ▏
    case 0x2594: ctx.fillRect(x, y, w, R(h / 8)); break                       // ▔ upper 1/8
    case 0x2595: ctx.fillRect(x + R(w * 7 / 8), y, w - R(w * 7 / 8), h); break // ▕ right 1/8
    case 0x2591: ctx.globalAlpha = 0.25; ctx.fillRect(x, y, w, h); break       // ░
    case 0x2592: ctx.globalAlpha = 0.5; ctx.fillRect(x, y, w, h); break        // ▒
    case 0x2593: ctx.globalAlpha = 0.75; ctx.fillRect(x, y, w, h); break       // ▓
    case 0x2596: ctx.fillRect(x, y + hy, hx, h - hy); break                    // ▖
    case 0x2597: ctx.fillRect(x + hx, y + hy, w - hx, h - hy); break           // ▗
    case 0x2598: ctx.fillRect(x, y, hx, hy); break                            // ▘
    case 0x2599: ctx.fillRect(x, y, hx, hy); ctx.fillRect(x, y + hy, w - hx + hx, h - hy); break // ▙ (left col + bottom)
    case 0x259a: ctx.fillRect(x, y, hx, hy); ctx.fillRect(x + hx, y + hy, w - hx, h - hy); break // ▚
    case 0x259b: ctx.fillRect(x, y, w, hy); ctx.fillRect(x, y + hy, hx, h - hy); break // ▛
    case 0x259c: ctx.fillRect(x, y, w, hy); ctx.fillRect(x + hx, y + hy, w - hx, h - hy); break // ▜
    case 0x259d: ctx.fillRect(x + hx, y, w - hx, hy); break                    // ▝
    case 0x259e: ctx.fillRect(x + hx, y, w - hx, hy); ctx.fillRect(x, y + hy, hx, h - hy); break // ▞
    case 0x259f: ctx.fillRect(x + hx, y, w - hx, hy); ctx.fillRect(x, y + hy, w, h - hy); break // ▟
    default: ctx.restore(); return false
  }
  ctx.restore()
  return true
}

// Box Drawing (U+2500–U+257F): drawn as line segments from the cell center to
// its edges, like xterm's customGlyphs. The font (Menlo) lacks the rounded
// corners ╭╮╰╯, so without this they fall back to .notdef boxes / garbage.
// Direction bitmask: 1=left 2=right 4=up 8=down (heavy/double rendered as light).
const BOX_DIRS: Record<number, number> = {
  0x2500: 1 | 2, 0x2501: 1 | 2, 0x2502: 4 | 8, 0x2503: 4 | 8,
  0x250c: 2 | 8, 0x250f: 2 | 8, 0x2510: 1 | 8, 0x2513: 1 | 8,
  0x2514: 2 | 4, 0x2517: 2 | 4, 0x2518: 1 | 4, 0x251b: 1 | 4,
  0x251c: 4 | 8 | 2, 0x2523: 4 | 8 | 2, 0x2524: 4 | 8 | 1, 0x252b: 4 | 8 | 1,
  0x252c: 1 | 2 | 8, 0x2533: 1 | 2 | 8, 0x2534: 1 | 2 | 4, 0x253b: 1 | 2 | 4,
  0x253c: 1 | 2 | 4 | 8, 0x254b: 1 | 2 | 4 | 8,
  0x2574: 1, 0x2575: 4, 0x2576: 2, 0x2577: 8,
  0x256d: 2 | 8, 0x256e: 1 | 8, 0x256f: 1 | 4, 0x2570: 2 | 4,
  0x2550: 1 | 2, 0x2551: 4 | 8, 0x2554: 2 | 8, 0x2557: 1 | 8,
  0x255a: 2 | 4, 0x255d: 1 | 4, 0x2560: 4 | 8 | 2, 0x2563: 4 | 8 | 1,
  0x2566: 1 | 2 | 8, 0x2569: 1 | 2 | 4, 0x256c: 1 | 2 | 4 | 8,
}
function drawBoxGlyph(ctx: CanvasRenderingContext2D, cp: number, x: number, y: number, w: number, h: number): boolean {
  const m = BOX_DIRS[cp]
  if (m === undefined) return false
  const t = Math.max(1, Math.round(h / 10))
  const ht = Math.floor(t / 2)
  const midX = x + Math.round(w / 2), midY = y + Math.round(h / 2)
  ctx.fillStyle = '#fff'
  if (m & 1) ctx.fillRect(x, midY - ht, midX - x + ht, t)                   // left → center
  if (m & 2) ctx.fillRect(midX - ht, midY - ht, x + w - (midX - ht), t)     // right
  if (m & 4) ctx.fillRect(midX - ht, y, t, midY - y + ht)                   // up
  if (m & 8) ctx.fillRect(midX - ht, midY - ht, t, y + h - (midY - ht))     // down
  return true
}

function compile(gl: WebGL2RenderingContext, type: number, src: string): WebGLShader {
  const s = gl.createShader(type)!
  gl.shaderSource(s, src)
  gl.compileShader(s)
  if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
    throw new Error('shader: ' + gl.getShaderInfoLog(s))
  }
  return s
}

export class SingleCanvasRenderer {
  readonly gl: WebGL2RenderingContext
  cellW: number
  cellH: number
  private prog: WebGLProgram
  private instanceVBO: WebGLBuffer
  private instanceData = new Float32Array(0)
  private atlasTex: WebGLTexture
  private atlasCanvas: HTMLCanvasElement
  private atlasCtx: CanvasRenderingContext2D
  // Keyed by codepoint (number), NOT string: the hot path looks up a glyph per
  // cell per frame, and a number key avoids allocating a String.fromCodePoint
  // string every lookup (that allocation churned GC and caused ~50ms pauses).
  private glyphSlots = new Map<number, number>()
  // Slot 0 is reserved BLANK (never rasterised) so the cursor — which samples
  // atlas UV (0,0) — is a solid block, not the first glyph that happened to land
  // in slot 0. Real glyphs start at slot 1.
  private nextSlot = 1
  private glyphsPerRow: number
  private atlasDirty = false
  private uniforms: Record<string, WebGLUniformLocation | null> = {}
  // Transparent mode: clear to alpha 0 so the canvas composites over the DOM
  // (the production renderer floats over the pane layout; empty cells let the
  // pane's own background show through). Opaque mode (spike) clears to `bg`.
  private transparent: boolean

  constructor(canvas: HTMLCanvasElement, opts: { fontFamily: string; fontSize: number; dpr: number; alpha?: boolean; lineHeight?: number }) {
    this.transparent = opts.alpha ?? false
    const gl = canvas.getContext('webgl2', { alpha: this.transparent, antialias: false, depth: false, premultipliedAlpha: true })
    if (!gl) throw new Error('WebGL2 not available')
    this.gl = gl

    // Measure the monospace cell at device resolution.
    const probe = document.createElement('canvas').getContext('2d')!
    const fontPx = Math.round(opts.fontSize * opts.dpr)
    probe.font = `${fontPx}px ${opts.fontFamily}`
    const m = probe.measureText('M')
    this.cellW = Math.ceil(m.width)
    this.cellH = Math.ceil(fontPx * (opts.lineHeight ?? 1.3))

    // Glyph atlas.
    this.atlasCanvas = document.createElement('canvas')
    this.atlasCanvas.width = ATLAS_SIZE
    this.atlasCanvas.height = ATLAS_SIZE
    this.atlasCtx = this.atlasCanvas.getContext('2d')!
    this.atlasCtx.font = `${fontPx}px ${opts.fontFamily}`
    // Center glyphs vertically in the cell (the cell is the font's full line
    // box, taller than the glyph) so text sits like xterm rather than hugging
    // the top. Block elements are drawn as rects (edge-anchored) separately.
    this.atlasCtx.textBaseline = 'middle'
    this.atlasCtx.textAlign = 'left'
    this.atlasCtx.fillStyle = '#fff'
    this.glyphsPerRow = Math.floor(ATLAS_SIZE / this.cellW)

    // Program.
    this.prog = gl.createProgram()!
    gl.attachShader(this.prog, compile(gl, gl.VERTEX_SHADER, VERT))
    gl.attachShader(this.prog, compile(gl, gl.FRAGMENT_SHADER, FRAG))
    gl.linkProgram(this.prog)
    if (!gl.getProgramParameter(this.prog, gl.LINK_STATUS)) {
      throw new Error('link: ' + gl.getProgramInfoLog(this.prog))
    }
    gl.useProgram(this.prog)
    for (const u of ['uCanvas', 'uCell', 'uGlyph', 'uAtlas']) {
      this.uniforms[u] = gl.getUniformLocation(this.prog, u)
    }

    // Unit quad (triangle strip).
    const quad = gl.createBuffer()!
    gl.bindBuffer(gl.ARRAY_BUFFER, quad)
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([0, 0, 1, 0, 0, 1, 1, 1]), gl.STATIC_DRAW)
    gl.enableVertexAttribArray(0)
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0)

    // Instance buffer (stride 10 floats).
    this.instanceVBO = gl.createBuffer()!
    gl.bindBuffer(gl.ARRAY_BUFFER, this.instanceVBO)
    const stride = 10 * 4
    const attrs: [number, number, number][] = [[1, 2, 0], [2, 2, 2], [3, 3, 4], [4, 3, 7]]
    for (const [loc, size, off] of attrs) {
      gl.enableVertexAttribArray(loc)
      gl.vertexAttribPointer(loc, size, gl.FLOAT, false, stride, off * 4)
      gl.vertexAttribDivisor(loc, 1)
    }

    // Atlas texture.
    this.atlasTex = gl.createTexture()!
    gl.bindTexture(gl.TEXTURE_2D, this.atlasTex)
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, ATLAS_SIZE, ATLAS_SIZE, 0, gl.RGBA, gl.UNSIGNED_BYTE, null)
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST)
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST)
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE)
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE)

    gl.uniform2f(this.uniforms.uCell!, this.cellW, this.cellH)
    gl.uniform2f(this.uniforms.uGlyph!, this.cellW / ATLAS_SIZE, this.cellH / ATLAS_SIZE)
    gl.uniform1i(this.uniforms.uAtlas!, 0)
  }

  resize(deviceW: number, deviceH: number) {
    this.gl.canvas.width = deviceW
    this.gl.canvas.height = deviceH
    this.gl.viewport(0, 0, deviceW, deviceH)
    this.gl.uniform2f(this.uniforms.uCanvas!, deviceW, deviceH)
  }

  /** Override the cell size (device px) — e.g. to match the host terminal's
   *  measured cell exactly so the grid fits its pane. Resets the glyph atlas so
   *  glyphs re-rasterise into the new cell, and updates the cell/glyph uniforms. */
  setCell(cellW: number, cellH: number) {
    if (cellW < 1 || cellH < 1 || (cellW === this.cellW && cellH === this.cellH)) return
    this.cellW = cellW
    this.cellH = cellH
    this.glyphsPerRow = Math.floor(ATLAS_SIZE / cellW)
    this.glyphSlots.clear()
    this.nextSlot = 1 // keep slot 0 blank for the cursor
    this.atlasCtx.clearRect(0, 0, ATLAS_SIZE, ATLAS_SIZE)
    this.atlasDirty = true
    const gl = this.gl
    gl.useProgram(this.prog)
    gl.uniform2f(this.uniforms.uCell!, cellW, cellH)
    gl.uniform2f(this.uniforms.uGlyph!, cellW / ATLAS_SIZE, cellH / ATLAS_SIZE)
  }

  /** Atlas UV (top-left, normalised) for a glyph by codepoint, rasterising on
   *  first use. `String.fromCodePoint` runs only when a new glyph is added. */
  glyphUV(cp: number, out: { u: number; v: number }): void {
    let slot = this.glyphSlots.get(cp)
    if (slot === undefined) {
      slot = this.nextSlot++
      this.glyphSlots.set(cp, slot)
      const col = slot % this.glyphsPerRow
      const row = Math.floor(slot / this.glyphsPerRow)
      const gx = col * this.cellW, gy = row * this.cellH
      if (!drawBlockGlyph(this.atlasCtx, cp, gx, gy, this.cellW, this.cellH) &&
          !drawBoxGlyph(this.atlasCtx, cp, gx, gy, this.cellW, this.cellH)) {
        // textBaseline 'middle' → y at the cell's vertical center.
        this.atlasCtx.fillText(String.fromCodePoint(cp), gx, gy + this.cellH / 2)
      }
      this.atlasDirty = true
    }
    const col = slot % this.glyphsPerRow
    const row = Math.floor(slot / this.glyphsPerRow)
    out.u = (col * this.cellW) / ATLAS_SIZE
    out.v = (row * this.cellH) / ATLAS_SIZE
  }

  /** Draw `count` instances from `data` (stride-10 floats). Clears to `bg` first. */
  draw(data: Float32Array, count: number, bg: [number, number, number]) {
    const gl = this.gl
    if (this.atlasDirty) {
      gl.bindTexture(gl.TEXTURE_2D, this.atlasTex)
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, this.atlasCanvas)
      this.atlasDirty = false
    }
    if (this.transparent) gl.clearColor(0, 0, 0, 0)
    else gl.clearColor(bg[0], bg[1], bg[2], 1)
    gl.clear(gl.COLOR_BUFFER_BIT)
    if (count === 0) return
    gl.bindBuffer(gl.ARRAY_BUFFER, this.instanceVBO)
    gl.bufferData(gl.ARRAY_BUFFER, data.subarray(0, count * 10), gl.DYNAMIC_DRAW)
    gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, count)
  }

  /** Grow/return the reusable instance buffer (stride-10 floats). */
  ensureCapacity(cells: number): Float32Array {
    if (this.instanceData.length < cells * 10) {
      this.instanceData = new Float32Array(cells * 10)
    }
    return this.instanceData
  }
}
