import { defineStore } from 'pinia'
import { ref } from 'vue'

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

  return { alwaysShowFooter, hideUsageBar, hideSonnetUsage, defaultShell, overviewClaudeOnly, useCustomTerminalBg }
})
