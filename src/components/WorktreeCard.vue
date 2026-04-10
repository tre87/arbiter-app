<script setup lang="ts">
import { computed } from 'vue'
import RobotIcon from './RobotIcon.vue'
import MdiIcon from './MdiIcon.vue'
import ClaudeIcon from './ClaudeIcon.vue'
import { mdiBellRing, mdiCogPlay, mdiSourceMerge, mdiClose, mdiConsole } from '@mdi/js'
import type { WorktreeClaudeStatus } from '../stores/project'

const props = defineProps<{
  branchName: string
  isMain: boolean
  isActive: boolean
  isMerged: boolean
  status: WorktreeClaudeStatus
}>()

const emit = defineEmits<{
  click: []
  end: []
  remove: []
  contextmenu: [event: MouseEvent]
}>()

function onClick() {
  if (props.isMerged) return
  emit('click')
}

function onContextMenu(e: MouseEvent) {
  if (props.isMain) return
  e.preventDefault()
  emit('contextmenu', e)
}

const statusLabel = computed(() => {
  switch (props.status.status) {
    case 'working': return 'Working...'
    case 'attention': return 'Needs attention'
    case 'ready': return 'Idle'
    case 'exited': return 'Terminal'
    default: return 'Terminal'
  }
})

const statusClass = computed(() => `status-${props.status.status}`)

// Claude stats (model, tokens, context) only make sense when Claude is alive.
const claudeActive = computed(() => {
  const s = props.status.status
  return s === 'ready' || s === 'working' || s === 'attention'
})

const modelInfo = computed(() => {
  const id = props.status.model
  if (!id) return { name: '', cls: '' }
  const m = id.match(/(opus|sonnet|haiku|flash)[- ]?(\d+)[- ]?(\d+)?/)
  if (m) {
    const family = m[1].charAt(0).toUpperCase() + m[1].slice(1)
    const ver = m[3] ? `${m[2]}.${m[3]}` : m[2]
    return { name: `${family} ${ver}`, cls: m[1] }
  }
  return { name: id.replace('claude-', ''), cls: '' }
})

function contextWindow(id: string | null): number {
  if (!id) return 200_000
  return 200_000
}
const contextMaxLabel = computed(() => (contextWindow(props.status.model) / 1000) + 'k')

const tokenDisplay = computed(() => {
  const total = props.status.inputTokens + props.status.outputTokens +
    props.status.cacheReadTokens + props.status.cacheWriteTokens
  if (total === 0) return ''
  if (total >= 1000000) return `${(total / 1000000).toFixed(1)}M`
  if (total >= 1000) return `${(total / 1000).toFixed(0)}k`
  return String(total)
})

const contextPercent = computed(() => Math.min(100, Math.max(0, props.status.contextPercent)))
const progressColor = computed(() => {
  if (contextPercent.value > 80) return 'var(--color-danger)'
  if (contextPercent.value > 60) return 'var(--color-warning)'
  return 'var(--color-success)'
})
</script>

<template>
  <div
    class="worktree-card"
    :class="{ active: isActive, merged: isMerged }"
    @click="onClick"
    @contextmenu="onContextMenu"
  >
    <div class="card-header">
      <div class="card-icon-area">
        <RobotIcon :branch-name="branchName" :size="32" :animated="status.status === 'working' && !isMerged" />
      </div>
      <div class="card-info">
        <div class="card-name-row">
          <span class="branch-name">{{ branchName }}</span>
          <span class="spacer" />
          <span v-if="!isMerged && claudeActive && modelInfo.name" class="model-label" :class="'model-' + modelInfo.cls">{{ modelInfo.name }}</span>
        </div>
        <div class="card-status-row">
          <span v-if="isMerged" class="status-badge status-merged">
            <MdiIcon :path="mdiSourceMerge" :size="12" />
            Merged
          </span>
          <span v-else class="status-badge" :class="statusClass">
            <MdiIcon v-if="status.status === 'working'" :path="mdiCogPlay" :size="12" class="icon-spin" />
            <MdiIcon v-else-if="status.status === 'attention'" :path="mdiBellRing" :size="12" class="icon-ring" />
            <ClaudeIcon v-else-if="status.status === 'ready'" :size="12" />
            <MdiIcon v-else :path="mdiConsole" :size="12" />
            {{ statusLabel }}
          </span>
          <span v-if="!isMerged && claudeActive && tokenDisplay" class="token-count">{{ tokenDisplay }}</span>
          <span class="spacer" />
          <span v-if="!isMerged && claudeActive" class="context-pct">
            {{ Math.round(contextPercent) }}%<span class="context-max">/{{ contextMaxLabel }}</span>
          </span>
        </div>
        <div v-if="!isMerged && claudeActive" class="usage-bar">
          <div class="usage-fill" :style="{ width: contextPercent + '%', background: progressColor }" />
        </div>
      </div>
    </div>
    <button
      v-if="isMerged"
      class="end-btn merged-remove"
      title="Remove merged worktree"
      @click.stop="emit('remove')"
    >
      <MdiIcon :path="mdiClose" :size="14" />
    </button>
    <button v-else-if="!isMain" class="end-btn" title="End worktree session" @click.stop="emit('end')">
      &times;
    </button>
  </div>
</template>

<style scoped>
.worktree-card {
  display: flex;
  align-items: center;
  padding: 8px 10px;
  border-radius: 6px;
  background: var(--color-bg-subtle);
  border: 1px solid var(--color-card-border);
  cursor: pointer;
  transition: border-color 0.1s, background 0.1s;
  position: relative;
}
.worktree-card:hover {
  background: var(--color-bg-elevated);
}
.worktree-card.active {
  background: rgba(86, 156, 214, 0.12);
}
.worktree-card.active .branch-name {
  color: var(--azure);
}
.worktree-card.merged {
  cursor: default;
  opacity: 0.6;
  background: var(--color-bg-subtle);
  border-color: var(--color-card-border);
}
.worktree-card.merged:hover {
  background: var(--color-bg-subtle);
}
.worktree-card.merged .branch-name {
  text-decoration: line-through;
  text-decoration-color: var(--color-text-muted);
}

.card-header {
  display: flex;
  align-items: center;
  gap: 10px;
  flex: 1;
  min-width: 0;
}

.card-icon-area {
  width: 32px;
  height: 32px;
  flex-shrink: 0;
  display: flex;
  align-items: center;
  justify-content: center;
}

.card-info {
  flex: 1;
  min-width: 0;
}

.card-name-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.branch-name {
  font-weight: 600;
  font-size: 13px;
  color: var(--color-text-muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  transition: color 0.12s;
}

.spacer { flex: 1; }

.usage-bar {
  height: 3px;
  background: var(--color-bg);
  border-radius: 2px;
  margin-top: 4px;
  overflow: hidden;
}
.usage-fill {
  height: 100%;
  border-radius: 2px;
  transition: width 0.3s, background 0.3s;
}

.context-pct {
  font-size: 11px;
  color: #569cd6;
  font-weight: 600;
  white-space: nowrap;
}
.context-max {
  color: var(--color-text-muted);
  opacity: 0.6;
  font-weight: 400;
}

.card-status-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 2px;
}

.status-badge {
  display: flex;
  align-items: center;
  gap: 4px;
  font-size: 11px;
  font-weight: 600;
  color: var(--color-text-secondary);
}
.status-working   { color: var(--azure); }
.status-attention { color: #e5a03c; }
.status-ready     { color: var(--color-text-muted); }
.status-exited    { color: var(--color-text-muted); opacity: 0.7; }
.status-merged    { color: #a371f7; }

.icon-spin {
  animation: spin 1.6s linear infinite;
  transform-origin: center;
}
.icon-ring {
  animation: ring 1.2s ease-in-out infinite;
  transform-origin: top center;
}
@keyframes ring {
  0%, 100% { transform: rotate(0); }
  20% { transform: rotate(-12deg); }
  40% { transform: rotate(10deg); }
  60% { transform: rotate(-6deg); }
  80% { transform: rotate(4deg); }
}

.model-label {
  font-size: 11px;
  font-weight: 600;
  color: var(--color-text-muted);
}
.model-sonnet { color: #9cdcfe; }
.model-opus   { color: #4ec9b0; }
.model-haiku  { color: #b5cea8; }
.model-flash  { color: #c678dd; }

.token-count {
  font-size: 11px;
  color: var(--color-text-muted);
}

.end-btn {
  position: absolute;
  top: 4px;
  right: 4px;
  background: none;
  border: none;
  color: var(--color-text-muted);
  font-size: 14px;
  cursor: pointer;
  padding: 0 4px;
  line-height: 1;
  opacity: 0;
  transition: opacity 0.1s;
}
.worktree-card:hover .end-btn {
  opacity: 1;
}
.end-btn:hover {
  color: var(--color-danger);
}
.end-btn.merged-remove {
  opacity: 1;
  color: var(--color-text-muted);
}
.end-btn.merged-remove:hover {
  color: var(--color-danger);
}

@keyframes spin {
  to { transform: rotate(360deg); }
}
</style>
