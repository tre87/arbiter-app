import type { ITheme } from '@xterm/xterm'

// Arbiter's signature background — used when the user prefers the custom
// look over the platform theme's own background.
export const CUSTOM_TERMINAL_BG = '#121212'

// iTerm2 default palette. Also a close match for Terminal.app "Basic" — picked
// as the macOS default because it's the most widely recognized "Mac terminal" look.
export const iterm2DefaultTheme: ITheme = {
  background: '#000000',
  foreground: '#ffffff',
  cursor: '#c7c7c7',
  cursorAccent: '#000000',
  selectionBackground: 'rgba(255,255,255,0.25)',
  black: '#000000',         brightBlack: '#686868',
  red: '#c91b00',           brightRed: '#ff6e67',
  green: '#00c200',         brightGreen: '#5ffa68',
  yellow: '#c7c400',        brightYellow: '#fffc67',
  blue: '#0225c7',          brightBlue: '#6871ff',
  magenta: '#ca30c7',       brightMagenta: '#ff77ff',
  cyan: '#00c5c7',          brightCyan: '#60fdff',
  white: '#c7c7c7',         brightWhite: '#ffffff',
}

// Campbell — the default scheme used by modern Windows Terminal and PowerShell.
export const campbellTheme: ITheme = {
  background: '#0c0c0c',
  foreground: '#cccccc',
  cursor: '#ffffff',
  cursorAccent: '#0c0c0c',
  selectionBackground: 'rgba(255,255,255,0.25)',
  black: '#0c0c0c',         brightBlack: '#767676',
  red: '#c50f1f',           brightRed: '#e74856',
  green: '#13a10e',         brightGreen: '#16c60c',
  yellow: '#c19c00',        brightYellow: '#f9f1a5',
  blue: '#0037da',          brightBlue: '#3b78ff',
  magenta: '#881798',       brightMagenta: '#b4009e',
  cyan: '#3a96dd',          brightCyan: '#61d6d6',
  white: '#cccccc',         brightWhite: '#f2f2f2',
}

export function pickPlatformTheme(): ITheme {
  if (typeof navigator === 'undefined') return iterm2DefaultTheme
  if (navigator.platform.startsWith('Win')) return campbellTheme
  return iterm2DefaultTheme
}
