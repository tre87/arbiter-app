<script setup lang="ts">
import { ref, watch, onMounted, onBeforeUnmount } from 'vue'
import { useUsageStore } from '../stores/usage'
import PulseLoader from './PulseLoader.vue'

const store = useUsageStore()

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

onMounted(() => {
  store.startPolling()
  // Wall-clock countdown display — must tick each second; no event source.
  cdTimer = setInterval(() => {
    countdown.value = countdown.value > 0 ? countdown.value - 1 : 120
  }, 1000)
})

onBeforeUnmount(() => {
  store.stopPolling()
  if (cdTimer) clearInterval(cdTimer)
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

  <!-- Multi-org account with no saved selection -->
  <template v-else-if="store.needsOrgSelection">
    <span class="muted-label">Choose organization</span>
    <button class="stats-btn accent" @click="store.openOrgPicker()">Select</button>
  </template>

  <!-- Stats -->
  <template v-else-if="store.data">
    <span class="plan-badge">{{ store.data.plan }}</span>

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
  height: 26px;
}

.stat-label {
  font-size: 11px;
  color: var(--color-text-secondary);
  white-space: nowrap;
}

.bar-track {
  position: relative;
  width: 72px;
  height: 18px;
  background: var(--color-bg);
  border-radius: var(--radius-sm);
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
  font-variant-numeric: tabular-nums;
}

.stat-reset {
  font-size: 11px;
  color: var(--color-text-secondary);
  white-space: nowrap;
  font-variant-numeric: tabular-nums;
}

.plan-badge {
  font-size: 10px;
  font-weight: 600;
  color: var(--color-accent);
  letter-spacing: 0.08em;
  text-transform: uppercase;
  line-height: 1;
  border: 1px solid var(--color-card-border);
  border-radius: var(--radius-md);
  padding: 5px 7px;
  height: 26px;
  box-sizing: border-box;
  display: flex;
  align-items: center;
}

.refresh-btn {
  display: flex;
  align-items: center;
  gap: 4px;
  background: none;
  border: 1px solid var(--color-card-border);
  border-radius: var(--radius-md);
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
  color: var(--color-text-secondary);
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
