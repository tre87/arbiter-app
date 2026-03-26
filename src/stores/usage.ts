import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

export interface UsagePeriod {
  utilization: number
  resets_at: string | null
}

export interface UsageData {
  five_hour: UsagePeriod | null
  seven_day: UsagePeriod | null
  seven_day_opus: UsagePeriod | null
  seven_day_sonnet: UsagePeriod | null
  plan: string
}

export const useUsageStore = defineStore('usage', () => {
  const data = ref<UsageData | null>(null)
  const loading = ref(false)
  const pending = ref(true)   // true until first WebView response arrives
  const needsLogin = ref(false)
  const error = ref<string | null>(null)

  let pollTimer: ReturnType<typeof setInterval> | null = null
  let unlisten: (() => void) | null = null

  async function fetch() {
    loading.value = true
    error.value = null
    try {
      data.value = await invoke<UsageData>('get_usage')
      pending.value = false
      needsLogin.value = false
    } catch (e: unknown) {
      const msg = String(e)
      if (msg.includes('needs_login')) {
        pending.value = false
        needsLogin.value = true
      } else if (msg.includes('pending')) {
        // WebView still loading — stay in pending state, keep polling
      } else if (!data.value) {
        error.value = msg
      }
    } finally {
      loading.value = false
    }
  }

  async function openLogin() {
    await invoke('open_login_window')
  }

  function startPolling() {
    fetch()
    // Poll every 2s while pending (WebView still loading), then settle to 2min
    pollTimer = setInterval(() => {
      if (pending.value) fetch()
    }, 2_000)

    listen<void>('usage-updated', () => {
      fetch()
      // Switch to slow polling once we have a real response
      if (pollTimer) { clearInterval(pollTimer); pollTimer = setInterval(fetch, 120_000) }
    }).then(fn => { unlisten = fn })
  }

  function stopPolling() {
    if (pollTimer) { clearInterval(pollTimer); pollTimer = null }
    if (unlisten) { unlisten(); unlisten = null }
  }

  function formatReset(period: UsagePeriod | null): string {
    if (!period?.resets_at) return ''
    const ms = new Date(period.resets_at).getTime() - Date.now()
    if (ms <= 0) return 'now'
    const d = Math.floor(ms / 86_400_000)
    const h = Math.floor((ms % 86_400_000) / 3_600_000)
    const m = Math.floor((ms % 3_600_000) / 60_000)
    if (d > 0) return `${d}d ${h}h`
    if (h > 0) return `${h}h ${m}m`
    return `${m}m`
  }

  const primaryReset = computed(() => formatReset(data.value?.five_hour ?? null))

  return {
    data, loading, pending, needsLogin, error,
    fetch, openLogin, startPolling, stopPolling,
    formatReset, primaryReset,
  }
})
