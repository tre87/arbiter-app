import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

export interface UsagePeriod {
  utilization: number
  resets_at: string | null
}

export interface OrgInfo {
  uuid: string
  name: string
}

export interface UsageData {
  five_hour: UsagePeriod | null
  seven_day: UsagePeriod | null
  seven_day_opus: UsagePeriod | null
  seven_day_sonnet: UsagePeriod | null
  plan: string
  account_email: string | null
  account_name: string | null
  org_name: string | null
  has_multiple_orgs: boolean
}

export const useUsageStore = defineStore('usage', () => {
  const data = ref<UsageData | null>(null)
  const loading = ref(false)
  const pending = ref(true)   // true until first WebView response arrives
  const needsLogin = ref(false)
  const needsOrgSelection = ref(false)
  const pickerOpen = ref(false)
  const availableOrgs = ref<OrgInfo[]>([])
  const error = ref<string | null>(null)

  let unlisten: (() => void) | null = null

  async function fetch() {
    loading.value = true
    error.value = null
    try {
      data.value = await invoke<UsageData>('get_usage')
      pending.value = false
      needsLogin.value = false
      needsOrgSelection.value = false
    } catch (e: unknown) {
      const msg = String(e)
      if (msg.includes('needs_login')) {
        pending.value = false
        needsLogin.value = true
        needsOrgSelection.value = false
      } else if (msg.includes('needs_org_selection')) {
        pending.value = false
        needsLogin.value = false
        needsOrgSelection.value = true
        // Pull the list so the dialog can render immediately
        try { availableOrgs.value = await invoke<OrgInfo[]>('get_available_orgs') } catch { /* ignore */ }
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

  async function logout() {
    await invoke('logout_usage')
    data.value = null
    needsLogin.value = true
    needsOrgSelection.value = false
    availableOrgs.value = []
  }

  async function openOrgPicker() {
    // Manual open from Settings — load the list of orgs the script saw last
    try { availableOrgs.value = await invoke<OrgInfo[]>('get_available_orgs') } catch { /* ignore */ }
    pickerOpen.value = true
  }

  function closeOrgPicker() {
    pickerOpen.value = false
  }

  async function setSelectedOrg(org: OrgInfo) {
    await invoke('set_selected_org', { org })
    pickerOpen.value = false
    needsOrgSelection.value = false
    // Show the spinner until the WebView's refetch lands and emits usage-updated
    pending.value = true
    data.value = null
  }

  // Event-driven only. The backend's injected WebView script calls `report_usage`
  // on load and every 120s, which emits `usage-updated` — the sole refresh trigger.
  function startPolling() {
    fetch()
    listen<void>('usage-updated', () => { fetch() }).then(fn => { unlisten = fn })
  }

  function stopPolling() {
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

  // Dialog is visible whenever we are forced into selection OR the user opened it manually
  const orgPickerVisible = computed(() => needsOrgSelection.value || pickerOpen.value)

  return {
    data, loading, pending, needsLogin, needsOrgSelection, availableOrgs, pickerOpen, orgPickerVisible, error,
    fetch, openLogin, logout, startPolling, stopPolling,
    openOrgPicker, closeOrgPicker, setSelectedOrg,
    formatReset,
  }
})
