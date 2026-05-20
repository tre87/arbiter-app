import { defineStore } from 'pinia'
import { ref } from 'vue'

export const useDevSettingsStore = defineStore('devSettings', () => {
  const alwaysShowFooter = ref(false)
  const hideUsageBar = ref(false)
  const defaultShell = ref<'powershell' | 'gitbash'>('powershell')

  return { alwaysShowFooter, hideUsageBar, defaultShell }
})
