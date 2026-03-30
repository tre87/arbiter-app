<script setup lang="ts">
import { ref, computed, watch, onMounted, onBeforeUnmount } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { useUsageStore } from '../stores/usage'
import { useDevSettingsStore } from '../stores/devSettings'
import PulseLoader from './PulseLoader.vue'

const store = useUsageStore()
const devStore = useDevSettingsStore()
const osLocale = ref('en-US')

// Peak hours: weekdays 5am–11am PT (UTC-7 standard / UTC-8 daylight)
// We use America/Los_Angeles to handle DST automatically
const realPeakHours = ref(false)
const isPeakHours = computed(() => devStore.forcePeakHours || realPeakHours.value)

function checkPeakHours() {
  const now = new Date()
  const ptTime = new Date(now.toLocaleString('en-US', { timeZone: 'America/Los_Angeles' }))
  const day = ptTime.getDay() // 0=Sun, 6=Sat
  const hour = ptTime.getHours()
  realPeakHours.value = day >= 1 && day <= 5 && hour >= 5 && hour < 11
}

let peakTimer: ReturnType<typeof setInterval> | null = null

const peakTooltip = computed(() => {
  // Convert 5am and 11am PT to the user's local timezone
  // Use a fixed date (a Monday) to get the conversion right
  const base = new Date()
  const startPT = new Date(base.toLocaleString('en-US', { timeZone: 'America/Los_Angeles' }))
  startPT.setHours(5, 0, 0, 0)
  const endPT = new Date(base.toLocaleString('en-US', { timeZone: 'America/Los_Angeles' }))
  endPT.setHours(11, 0, 0, 0)

  // Compute offset between local and PT
  const localNow = base.getTime()
  const ptNow = new Date(base.toLocaleString('en-US', { timeZone: 'America/Los_Angeles' })).getTime()
  const offsetMs = localNow - ptNow

  const localStart = new Date(startPT.getTime() + offsetMs)
  const localEnd = new Date(endPT.getTime() + offsetMs)

  const fmt = (d: Date) => d.toLocaleTimeString(osLocale.value, { hour: 'numeric', minute: '2-digit' })

  return `Peak hours (weekdays ${fmt(localStart)}–${fmt(localEnd)}) — 5h session limits drain faster`
})

// Countdown to next auto-refresh
const countdown = ref(120)
let cdTimer: ReturnType<typeof setInterval> | null = null

function resetCountdown() {
  countdown.value = 120
}

function fmtCountdown(s: number) {
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`
}

// Reset whenever a fetch completes
watch(() => store.loading, (loading, was) => {
  if (was && !loading) resetCountdown()
})

onMounted(async () => {
  store.startPolling()
  cdTimer = setInterval(() => {
    countdown.value = countdown.value > 0 ? countdown.value - 1 : 120
  }, 1000)
  checkPeakHours()
  peakTimer = setInterval(checkPeakHours, 60_000)
  try {
    osLocale.value = await invoke<string>('get_locale')
  } catch { /* fallback to en-US */ }
})

onBeforeUnmount(() => {
  store.stopPolling()
  if (cdTimer) clearInterval(cdTimer)
  if (peakTimer) clearInterval(peakTimer)
})
</script>

<template>
  <!-- Waiting for initial WebView fetch -->
  <template v-if="store.pending">
    <PulseLoader />
  </template>

  <!-- Login needed (WebView got 401) -->
  <template v-else-if="store.needsLogin">
    <span class="muted-label">Not signed in</span>
    <button class="stats-btn accent" @click="store.openLogin()">Sign in</button>
  </template>

  <!-- Stats -->
  <template v-else-if="store.data">
    <span class="plan-badge">{{ store.data.plan }}</span>

    <span v-if="isPeakHours" class="peak-badge" :title="peakTooltip">⚡ PEAK</span>

    <!-- 5h -->
    <div v-if="store.data.five_hour" class="stat">
      <span class="stat-label">5h</span>
      <div class="bar-track">
        <div class="bar-fill blue" :style="{ width: store.data.five_hour.utilization + '%' }" />
        <span class="bar-text">{{ Math.round(store.data.five_hour.utilization) }}%</span>
      </div>
      <span class="stat-reset">{{ store.formatReset(store.data.five_hour) || '—' }}</span>
    </div>

    <!-- 7d -->
    <div v-if="store.data.seven_day" class="stat">
      <span class="stat-label">7d</span>
      <div class="bar-track">
        <div class="bar-fill green" :style="{ width: store.data.seven_day.utilization + '%' }" />
        <span class="bar-text">{{ Math.round(store.data.seven_day.utilization) }}%</span>
      </div>
      <span class="stat-reset">{{ store.formatReset(store.data.seven_day) || '—' }}</span>
    </div>

    <!-- Opus (Max plan) -->
    <div v-if="store.data.seven_day_opus" class="stat">
      <span class="stat-label">Opus</span>
      <div class="bar-track">
        <div class="bar-fill green" :style="{ width: store.data.seven_day_opus.utilization + '%' }" />
        <span class="bar-text">{{ Math.round(store.data.seven_day_opus.utilization) }}%</span>
      </div>
      <span class="stat-reset">{{ store.formatReset(store.data.seven_day_opus) || '—' }}</span>
    </div>

    <!-- Sonnet (Max plan) -->
    <div v-if="store.data.seven_day_sonnet" class="stat">
      <span class="stat-label">Sonnet</span>
      <div class="bar-track">
        <div class="bar-fill blue" :style="{ width: store.data.seven_day_sonnet.utilization + '%' }" />
        <span class="bar-text">{{ Math.round(store.data.seven_day_sonnet.utilization) }}%</span>
      </div>
      <span class="stat-reset">{{ store.formatReset(store.data.seven_day_sonnet) || '—' }}</span>
    </div>

    <button class="refresh-btn" title="Click to refresh" @click="store.fetch(); resetCountdown()">
      <span class="refresh-icon" :class="{ spinning: store.loading }">↺</span>
      <span class="refresh-cd">{{ fmtCountdown(countdown) }}</span>
    </button>
  </template>

  <!-- Error -->
  <template v-else-if="store.error">
    <span class="error-text" :title="store.error">Error</span>
    <button class="stats-btn" @click="store.fetch()">↺</button>
  </template>
</template>

<style scoped>
.muted-label {
  font-size: 11px;
  color: var(--color-text-muted);
}

.stat {
  display: flex;
  align-items: center;
  gap: 5px;
}

.stat-label {
  font-size: 11px;
  color: var(--color-text-muted);
  white-space: nowrap;
}

.bar-track {
  position: relative;
  width: 72px;
  height: 18px;
  background: var(--color-card-border);
  border-radius: 4px;
  overflow: hidden;
}

.bar-fill {
  position: absolute;
  inset: 0;
  border-radius: 3px;
  transition: width 0.4s ease;
}

.bar-fill.blue  { background: var(--color-accent); }
.bar-fill.green { background: var(--color-success); }

.bar-text {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 10px;
  font-weight: 600;
  color: #fff;
  text-shadow: 0 1px 2px rgba(0, 0, 0, 0.5);
  z-index: 1;
  pointer-events: none;
}

.stat-reset {
  font-size: 10px;
  color: var(--color-text-muted);
  white-space: nowrap;
  opacity: 0.7;
}

.plan-badge {
  font-size: 10px;
  font-weight: 600;
  color: var(--color-accent);
  letter-spacing: 0.08em;
  text-transform: uppercase;
  line-height: 1;
  border: 1px solid var(--color-card-border);
  border-radius: 4px;
  padding: 5px 7px;
  height: 26px;
  box-sizing: border-box;
  display: flex;
  align-items: center;
}

.peak-badge {
  font-size: 10px;
  font-weight: 600;
  color: var(--color-warning, #e8a735);
  letter-spacing: 0.08em;
  text-transform: uppercase;
  line-height: 1;
  border: 1px solid var(--color-warning, #e8a735);
  border-radius: 4px;
  padding: 5px 7px;
  height: 26px;
  box-sizing: border-box;
  display: flex;
  align-items: center;
  opacity: 0.9;
}

.refresh-btn {
  display: flex;
  align-items: center;
  gap: 4px;
  background: none;
  border: 1px solid var(--color-card-border);
  border-radius: 4px;
  padding: 5px 7px;
  height: 26px;
  box-sizing: border-box;
  cursor: pointer;
  -webkit-app-region: no-drag;
  transition: border-color 0.15s;
}

.refresh-btn:hover {
  border-color: var(--color-accent);
}

.refresh-btn:hover .refresh-icon,
.refresh-btn:hover .refresh-cd {
  color: var(--color-accent);
}

.refresh-icon {
  font-size: 12px;
  color: var(--color-text-muted);
  line-height: 1;
  transition: color 0.15s;
}

.refresh-icon.spinning {
  animation: spin 0.8s linear infinite;
}

@keyframes spin {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}

.refresh-cd {
  font-size: 11px;
  color: var(--color-text-muted);
  line-height: 1;
  font-variant-numeric: tabular-nums;
  transition: color 0.15s;
}

.stats-btn {
  background: none;
  border: none;
  color: var(--color-text-muted);
  cursor: pointer;
  font-size: 13px;
  padding: 0;
  line-height: 1;
  transition: color 0.15s;
  -webkit-app-region: no-drag;
}

.stats-btn:hover { color: var(--color-accent); }

.stats-btn.accent {
  border: 1px solid var(--color-accent);
  border-radius: 3px;
  color: var(--color-accent);
  font-size: 10px;
  font-family: inherit;
  padding: 2px 7px;
}

.error-text {
  font-size: 11px;
  color: var(--color-danger);
}
</style>
