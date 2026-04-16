import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import { WebLinksAddon } from '@xterm/addon-web-links'
import { WebglAddon } from '@xterm/addon-webgl'

export interface XtermInstance {
  term: Terminal
  fitAddon: FitAddon
  safeFit: () => void
  loadWebgl: () => void
  dispose: () => void
}

/** Create a configured xterm Terminal with FitAddon, WebLinksAddon, and a
 *  safeFit() that avoids FitAddon's circular css.cell.width issue. */
export function createXtermInstance(mountEl: HTMLElement): XtermInstance {
  const term = new Terminal({
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

  const fitAddon = new FitAddon()
  term.loadAddon(fitAddon)
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
      webglAddon.onContextLoss(() => {
        webglAddon?.dispose()
        webglAddon = null
      })
    } catch (e) {
      console.warn('WebGL addon failed, using DOM renderer:', e)
      webglAddon = null
    }
  }

  function safeFit() {
    const core = (term as any)._core
    const dw: number | undefined = core?._renderService?.dimensions?.device?.cell?.width
    const dh: number | undefined = core?._renderService?.dimensions?.device?.cell?.height
    if (!dw || !dh) {
      // Render service hasn't measured yet (e.g. right after a DOM
      // detach/reattach cycle). FitAddon.fit()'s circular css.cell.width
      // computation misfires here and resizes to absurdly small cols, which
      // then propagates to the PTY via onResize. Skip and let the next
      // ResizeObserver tick retry once dimensions are available.
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

    // Sanity clamp: if we would shrink dramatically, we're almost certainly
    // measuring during an unstable layout (workspace just unhidden, element
    // just reattached, transition in flight). Dropping a terminal from a
    // normal size down to a handful of columns would cause the PTY to emit
    // narrow-wrapped content that stays baked into scrollback forever, so
    // skip and wait for the next tick's measurement to stabilize.
    if (cols < 20 || rows < 5) return

    if (term.cols !== cols || term.rows !== rows) {
      term.resize(cols, rows)
    }
  }

  function dispose() {
    webglAddon?.dispose()
    webglAddon = null
    term.dispose()
  }

  return { term, fitAddon, safeFit, loadWebgl, dispose }
}
