import { defineStore } from 'pinia'
import { ref } from 'vue'

export const useDevSettingsStore = defineStore('devSettings', () => {
  const alwaysShowFooter = ref(false)
  const hideUsageBar = ref(false)
  const defaultShell = ref<'powershell' | 'gitbash'>('powershell')
  // Workspace overview: when true, list only terminals where Claude is
  // currently launched (any non-`closed` lifecycle). Default on.
  const overviewClaudeOnly = ref(true)

  return { alwaysShowFooter, hideUsageBar, defaultShell, overviewClaudeOnly }
})
