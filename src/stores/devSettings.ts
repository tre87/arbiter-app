import { defineStore } from 'pinia'
import { ref } from 'vue'

export const useDevSettingsStore = defineStore('devSettings', () => {
  const forcePeakHours = ref(false)

  return { forcePeakHours }
})
