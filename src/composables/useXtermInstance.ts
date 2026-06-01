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
  dispose: () => void
}

function buildTheme(useCustomBg: boolean): ITheme {
  const base = pickPlatformTheme()
  return useCustomBg ? { ...base, background: CUSTOM_TERMINAL_BG } : base
}

/** Create a configured xterm Terminal with WebLinksAddon and a safeFit() that
 *  computes cols/rows directly — FitAddon's circular css.cell.width derivation
 *  misfires on detach/reattach, so we don't use it. */
export function createXtermInstance(mountEl: HTMLElement): XtermInstance {
  const devStore = useDevSettingsStore()
  const term = new Terminal({
    theme: buildTheme(devStore.useCustomTerminalBg),
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

  // Live-update background when the toggle flips, so open terminals respond
  // immediately without needing a restart.
  const stopThemeWatcher = watch(
    () => devStore.useCustomTerminalBg,
    (useCustomBg) => { term.options.theme = buildTheme(useCustomBg) },
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
    stopThemeWatcher()
    webglAddon?.dispose()
    webglAddon = null
    term.dispose()
  }

  return { term, safeFit, loadWebgl, dispose }
}
