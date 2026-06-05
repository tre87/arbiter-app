import { defineStore } from 'pinia'
import { ref } from 'vue'

// Terminal scrollback (lines kept per terminal). Default matches iTerm2's 1000.
// Bounded rather than "unlimited": xterm-in-a-webview pays more memory + resize
// reflow per line than a native renderer, so a hard ceiling protects perf.
export const SCROLLBACK_DEFAULT = 1000
export const SCROLLBACK_MIN = 100
export const SCROLLBACK_MAX = 100_000

export function clampScrollback(n: number): number {
  if (!Number.isFinite(n)) return SCROLLBACK_DEFAULT
  return Math.min(SCROLLBACK_MAX, Math.max(SCROLLBACK_MIN, Math.round(n)))
}

export const useDevSettingsStore = defineStore('devSettings', () => {
  const alwaysShowFooter = ref(false)
  const hideUsageBar = ref(false)
  // Hide the per-model Sonnet bar in the usage stats; the 5h / 7d / Opus
  // numbers usually carry the relevant info and Sonnet is rarely the limit.
  const hideSonnetUsage = ref(true)
  const defaultShell = ref<'powershell' | 'gitbash'>('powershell')
  // Workspace overview: when true, list only terminals where Claude is
  // currently launched (any non-`closed` lifecycle). Default on.
  const overviewClaudeOnly = ref(true)
  // When true, override the platform terminal theme's background with
  // Arbiter's signature dark color. Default on.
  const useCustomTerminalBg = ref(true)
  // Hide the launch/continue Claude buttons in each terminal header.
  const hideClaudeButtons = ref(false)
  // Hide the PowerShell↔Git Bash toggle button in the terminal header
  // (Windows-only — there's no Git Bash to switch to elsewhere).
  const hideShellButton = ref(false)
  // Lines of scrollback kept per terminal. Lower = less memory + cheaper resize
  // reflow. Applied live to open terminals.
  const scrollback = ref(SCROLLBACK_DEFAULT)
  // Global perf/debug footer (toggle with Ctrl/Cmd+Shift+P). Runtime-only.
  const showDebugFooter = ref(false)
  // Single-canvas renderer spike overlay (Ctrl/Cmd+Shift+G). Runtime-only.
  const showRendererSpike = ref(false)
  // GPU single-canvas terminal renderer (Ctrl/Cmd+Shift+Y). Parses terminals in
  // Rust and draws all panes into one WebGL canvas instead of per-terminal
  // xterm. ON by default — only off when explicitly toggled off (persisted as
  // '0'). Read at pane mount, so the toggle reloads the app to apply cleanly.
  const useGpuRenderer = ref(localStorage.getItem('arbiter:gpuRenderer') !== '0')

  return { alwaysShowFooter, hideUsageBar, hideSonnetUsage, defaultShell, overviewClaudeOnly, useCustomTerminalBg, hideClaudeButtons, hideShellButton, scrollback, showDebugFooter, showRendererSpike, useGpuRenderer }
})
