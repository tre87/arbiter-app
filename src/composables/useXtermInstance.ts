import { watch } from 'vue'
import { Terminal, type ITheme } from '@xterm/xterm'
import { WebLinksAddon } from '@xterm/addon-web-links'
import { WebglAddon } from '@xterm/addon-webgl'
import { pickPlatformTheme, CUSTOM_TERMINAL_BG } from '../themes/terminalThemes'
import { useDevSettingsStore } from '../stores/devSettings'

export interface XtermInstance {
  term: Terminal
  safeFit: () => void
  loadWebgl: () => void
  unloadWebgl: () => void
  hasWebgl: () => boolean
  dispose: () => void
}

// Count of live WebGL contexts across all terminals — surfaced in the debug
// footer. WebKit caps these; if it exceeds the cap a visible terminal silently
// drops to the slow DOM renderer.
let webglContextCount = 0
export function activeWebglCount(): number {
  return webglContextCount
}

function buildTheme(useCustomBg: boolean): ITheme {
  const base = pickPlatformTheme()
  return useCustomBg ? { ...base, background: CUSTOM_TERMINAL_BG } : base
}

/** Create a configured xterm Terminal with WebLinksAddon and a safeFit() that
 *  computes cols/rows directly — FitAddon's circular css.cell.width derivation
 *  misfires on detach/reattach, so we don't use it. */
export function createXtermInstance(mountEl: HTMLElement, opts: { transparent?: boolean; bg?: string } = {}): XtermInstance {
  const devStore = useDevSettingsStore()
  // GPU mode renders xterm as the invisible input layer. `bg` forces a fixed
  // background (Arbiter's terminal color) so it never paints iTerm2 black, and
  // matches the GPU cells exactly. `transparent` is an alternative (unused).
  const themeFor = (useCustomBg: boolean): ITheme => {
    const base = buildTheme(useCustomBg)
    if (opts.transparent) return { ...base, background: 'rgba(0,0,0,0)' }
    if (opts.bg) return { ...base, background: opts.bg }
    return base
  }
  const term = new Terminal({
    theme: themeFor(devStore.useCustomTerminalBg),
    fontFamily: "Consolas, 'Cascadia Code', Menlo, 'SF Mono', monospace",
    fontSize: 12,
    lineHeight: 1.0,
    cursorBlink: false,
    cursorStyle: 'block',
    cursorInactiveStyle: 'outline',
    cursorWidth: 1,
    scrollback: devStore.scrollback,
    // Opaque: every terminal theme bg is a solid hex, so transparency buys no
    // visual change but makes WebGL render with an alpha channel and the
    // compositor blend each terminal over the full-window radial gradient every
    // frame. Off = opaque layers the compositor can skip behind → faster render.
    // (GPU mode forces it on — xterm is the transparent, content-free input layer.)
    allowTransparency: opts.transparent ?? false,
  })

  // Live-update background when the toggle flips, so open terminals respond
  // immediately without needing a restart.
  const stopThemeWatcher = watch(
    () => devStore.useCustomTerminalBg,
    (useCustomBg) => { term.options.theme = themeFor(useCustomBg) },
  )

  // Apply scrollback changes live to already-open terminals.
  const stopScrollbackWatcher = watch(
    () => devStore.scrollback,
    (n) => { term.options.scrollback = n },
  )

  term.loadAddon(new WebLinksAddon())
  term.open(mountEl)

  let webglAddon: WebglAddon | null = null

  function loadWebgl() {
    // Idempotent: already-loaded contexts stay loaded; contexts lost via
    // onContextLoss (webglAddon set to null there) get reloaded on the next
    // call. Lets callers safely invoke after a detach/reattach cycle.
    if (webglAddon) return
    try {
      webglAddon = new WebglAddon()
      term.loadAddon(webglAddon)
      webglContextCount++
      webglAddon.onContextLoss(() => {
        webglAddon?.dispose()
        webglAddon = null
        webglContextCount--
      })
    } catch (e) {
      console.warn('WebGL addon failed, using DOM renderer:', e)
      webglAddon = null
    }
  }

  // Release the GPU/WebGL context while the terminal is hidden (kept cached
  // across worktree/workspace switches). WebKit caps live WebGL contexts and
  // evicts the oldest when exceeded — which would silently drop a *visible*
  // heavy terminal to the slow DOM renderer. Freeing hidden terminals' contexts
  // keeps the live count ≈ the few visible panes. loadWebgl() re-acquires on the
  // next mount. The Terminal/scrollback are untouched.
  function unloadWebgl() {
    if (webglAddon) {
      webglAddon.dispose()
      webglAddon = null
      webglContextCount--
    }
  }

  function safeFit() {
    const core = (term as any)._core
    const dw: number | undefined = core?._renderService?.dimensions?.device?.cell?.width
    const dh: number | undefined = core?._renderService?.dimensions?.device?.cell?.height
    if (!dw || !dh) {
      // Render service hasn't measured yet (e.g. right after a DOM
      // detach/reattach cycle). Skip and let the next ResizeObserver tick
      // retry once dimensions are available — otherwise we'd resize to
      // absurdly small cols and SIGWINCH the PTY into narrow wrapping.
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

    const parentStyle = window.getComputedStyle(parent)
    const paddingLeft = parseFloat(parentStyle.paddingLeft) || 0
    const paddingRight = parseFloat(parentStyle.paddingRight) || 0

    const available = parentWidth - scrollbarWidth - paddingLeft - paddingRight
    let cols = Math.max(2, Math.floor(available / (dw / dpr)))
    const rows = Math.max(1, Math.floor(parentHeight / (dh / dpr)))

    while (cols > 2 && Math.round(dw * cols / dpr) > available) {
      cols--
    }

    // Sanity clamp on COLS only: a dramatic column drop almost always means
    // we're measuring during an unstable layout (workspace just unhidden,
    // element just reattached, transition in flight). Dropping a terminal from
    // a normal size down to a handful of columns makes the PTY emit
    // narrow-wrapped content that stays baked into scrollback forever, so skip
    // and wait for the next tick's measurement to stabilize.
    //
    // Rows are deliberately NOT clamped: shrinking rows only shortens the
    // viewport (a SIGWINCH height change, no scrollback reflow). Clamping them
    // used to make safeFit() bail on a genuinely short pane, leaving xterm with
    // a stale larger row count so the bottom (cursor) line rendered below the
    // visible viewport and disappeared. Letting rows fall to 1 keeps the
    // cursor line in view.
    if (cols < 20) return

    if (term.cols !== cols || term.rows !== rows) {
      term.resize(cols, rows)
    }
  }

  function dispose() {
    stopThemeWatcher()
    stopScrollbackWatcher()
    if (webglAddon) {
      webglAddon.dispose()
      webglAddon = null
      webglContextCount--
    }
    term.dispose()
  }

  return { term, safeFit, loadWebgl, unloadWebgl, hasWebgl: () => webglAddon !== null, dispose }
}
