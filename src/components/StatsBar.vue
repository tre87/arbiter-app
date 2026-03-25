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

  <!-- Stats -->
  <template v-else-if="store.data">
    <!-- 5h -->
    <div v-if="store.data.five_hour" class="stat">
      <span class="stat-label">5h</span>
      <div class="bar-track">
        <div class="bar-fill blue" :style="{ width: store.data.five_hour.utilization + '%' }" />
      </div>
      <span class="stat-pct">{{ Math.round(store.data.five_hour.utilization) }}%</span>
      <span class="stat-reset">{{ store.formatReset(store.data.five_hour) || '—' }}</span>
    </div>

    <!-- 7d -->
    <div v-if="store.data.seven_day" class="stat">
      <span class="stat-label">7d</span>
      <div class="bar-track">
        <div class="bar-fill green" :style="{ width: store.data.seven_day.utilization + '%' }" />
      </div>
      <span class="stat-pct">{{ Math.round(store.data.seven_day.utilization) }}%</span>
      <span class="stat-reset">{{ store.formatReset(store.data.seven_day) || '—' }}</span>
    </div>

    <!-- Opus (Max plan) -->
    <div v-if="store.data.seven_day_opus" class="stat">
      <span class="stat-label">Opus</span>
      <div class="bar-track">
        <div class="bar-fill green" :style="{ width: store.data.seven_day_opus.utilization + '%' }" />
      </div>
      <span class="stat-pct">{{ Math.round(store.data.seven_day_opus.utilization) }}%</span>
      <span class="stat-reset">{{ store.formatReset(store.data.seven_day_opus) || '—' }}</span>
    </div>

    <!-- Sonnet (Max plan) -->
    <div v-if="store.data.seven_day_sonnet" class="stat">
      <span class="stat-label">Sonnet</span>
      <div class="bar-track">
        <div class="bar-fill blue" :style="{ width: store.data.seven_day_sonnet.utilization + '%' }" />
      </div>
      <span class="stat-pct">{{ Math.round(store.data.seven_day_sonnet.utilization) }}%</span>
      <span class="stat-reset">{{ store.formatReset(store.data.seven_day_sonnet) || '—' }}</span>
    </div>

    <div class="vdivider" />
    <span class="plan-badge">{{ store.data.plan }}</span>

    <!-- Refresh button + countdown beneath -->
    <div class="refresh-group">
      <button class="stats-btn" title="Refresh" @click="store.fetch(); resetCountdown()">↺</button>
      <span class="refresh-cd">{{ fmtCountdown(countdown) }}</span>
    </div>
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

/* Each stat = 2-row grid: [label | bar | pct] / [_ | reset(centered) | _] */
.stat {
  display: grid;
  grid-template-columns: max-content 72px 28px;
  grid-template-rows: auto auto;
  row-gap: 3px;
  column-gap: 6px;
  align-items: center;
}

.stat-label {
  grid-column: 1;
  grid-row: 1;
  font-size: 11px;
  color: var(--color-text-muted);
  text-align: right;
}

.bar-track {
  grid-column: 2;
  grid-row: 1;
  height: 4px;
  background: var(--color-card-border);
  border-radius: 2px;
  overflow: hidden;
}

.bar-fill {
  height: 100%;
  border-radius: 2px;
  transition: width 0.4s ease;
}

.bar-fill.blue  { background: var(--color-accent); }
.bar-fill.green { background: var(--color-success); }

.stat-pct {
  grid-column: 3;
  grid-row: 1;
  font-size: 11px;
  color: var(--color-text-secondary);
  text-align: left;
}

.stat-reset {
  grid-column: 2;
  grid-row: 2;
  font-size: 11px;
  color: var(--color-text-muted);
  line-height: 1;
  text-align: center;
}

.vdivider {
  width: 1px;
  height: 18px;
  background: var(--color-card-border);
  flex-shrink: 0;
}

.plan-badge {
  font-size: 10px;
  font-weight: 600;
  color: var(--color-accent);
  letter-spacing: 0.06em;
  text-transform: uppercase;
}

.refresh-group {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 2px;
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

.refresh-cd {
  font-size: 11px;
  color: var(--color-text-muted);
  line-height: 1;
  font-variant-numeric: tabular-nums;
}

.error-text {
  font-size: 11px;
  color: var(--color-danger);
}
</style>
