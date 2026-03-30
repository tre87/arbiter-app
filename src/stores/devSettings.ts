import { defineStore } from 'pinia'
import { ref } from 'vue'

export const useDevSettingsStore = defineStore('devSettings', () => {
  const forcePeakHours = ref(false)
  const alwaysShowFooter = ref(false)
  const hideUsageBar = ref(false)
  const defaultShell = ref<'powershell' | 'gitbash'>('powershell')

  return { forcePeakHours, alwaysShowFooter, hideUsageBar, defaultShell }
})
