<script setup lang="ts">
defineProps<{
  sessionId: string | null
  model: string | null | undefined
  inputTokens: number
  outputTokens: number
  cacheWriteTokens: number
  cacheReadTokens: number
}>()

function modelLabel(id: string | null | undefined): string {
  if (!id) return ''
  const m = id.match(/(opus|sonnet|haiku|flash)[- ]?(\d+)[- ]?(\d+)?/)
  if (m) {
    const family = m[1].charAt(0).toUpperCase() + m[1].slice(1)
    const ver = m[3] ? `${m[2]}.${m[3]}` : m[2]
    return `${family} ${ver}`
  }
  return id.replace('claude-', '')
}
</script>

<template>
  <div class="info-panel">
    <div class="info-row">
      <span class="info-label">Session ID</span>
      <span class="info-value id-value">{{ sessionId ?? '—' }}</span>
    </div>
    <div v-if="model" class="info-row">
      <span class="info-label">Model</span>
      <span class="info-value">{{ modelLabel(model) }}</span>
    </div>
    <div class="info-row">
      <span class="info-label">Tokens in</span>
      <span class="info-value">{{ inputTokens.toLocaleString() }}</span>
    </div>
    <div class="info-row">
      <span class="info-label">Tokens out</span>
      <span class="info-value">{{ outputTokens.toLocaleString() }}</span>
    </div>
    <div class="info-row">
      <span class="info-label">Cache write</span>
      <span class="info-value">{{ cacheWriteTokens.toLocaleString() }}</span>
    </div>
    <div class="info-row">
      <span class="info-label">Cache read</span>
      <span class="info-value">{{ cacheReadTokens.toLocaleString() }}</span>
    </div>
  </div>
</template>

<style scoped>
.info-panel {
  position: absolute;
  top: 31px;
  right: 6px;
  z-index: 20;
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-card-border);
  border-radius: 4px;
  padding: 8px 12px;
  font-family: Consolas, 'Cascadia Code', Menlo, 'SF Mono', monospace;
  font-size: 11px;
  min-width: 220px;
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
}

.info-row {
  display: flex;
  justify-content: space-between;
  gap: 16px;
  padding: 3px 0;
}

.info-row + .info-row {
  border-top: 1px solid var(--color-card-border);
}

.info-label {
  color: var(--color-text-muted);
  opacity: 0.7;
  white-space: nowrap;
}

.info-value {
  color: var(--color-text-primary);
  text-align: right;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.id-value {
  color: #D97757;
  font-weight: 600;
  letter-spacing: 0.3px;
  font-size: 10px;
}
</style>
