<script setup lang="ts">
import MdiIcon from './MdiIcon.vue'
import { mdiSourceBranch, mdiFolderOutline } from '@mdi/js'

interface ClaudeSessionStatus {
  session_id: string
  model_id?: string | null
  input_tokens?: number | null
  output_tokens?: number | null
  cache_creation_input_tokens?: number | null
  cache_read_input_tokens?: number | null
  folder?: string | null
  branch?: string | null
}

defineProps<{ status: ClaudeSessionStatus | null }>()

function modelLabel(id: string | null | undefined): { name: string; cls: string } {
  if (!id) return { name: '', cls: '' }
  // Extract family + version from IDs like "claude-opus-4-6", "claude-sonnet-4-5-20251001"
  const m = id.match(/(opus|sonnet|haiku|flash)[- ]?(\d+)[- ]?(\d+)?/)
  if (m) {
    const family = m[1].charAt(0).toUpperCase() + m[1].slice(1)
    const ver = m[3] ? `${m[2]}.${m[3]}` : m[2]
    return { name: `${family} ${ver}`, cls: m[1] }
  }
  return { name: id.replace('claude-', ''), cls: '' }
}

function fmtK(n: number | null | undefined): string {
  if (n == null) return '0'
  if (n >= 1000) return (n / 1000).toFixed(1) + 'K'
  return String(n)
}
</script>

<template>
  <div class="terminal-footer">
    <template v-if="status">
      <span v-if="modelLabel(status.model_id).name" class="seg">
        <span :class="['model', 'model-' + modelLabel(status.model_id).cls]">{{ modelLabel(status.model_id).name }}</span>
      </span>

      <span class="divider">|</span>

      <span class="seg tok-seg">
        <span class="lbl">in:</span><span class="tok-in">{{ fmtK(status.input_tokens) }}</span>
        <span class="lbl"> out:</span><span class="tok-out">{{ fmtK(status.output_tokens) }}</span>
        <span class="lbl"> cw:</span><span class="tok-cw">{{ fmtK(status.cache_creation_input_tokens) }}</span>
        <span class="lbl"> cr:</span><span class="tok-cr">{{ fmtK(status.cache_read_input_tokens) }}</span>
      </span>

      <span class="spacer" />

      <span v-if="status.folder" class="seg folder-seg">
        <MdiIcon :path="mdiFolderOutline" :size="12" />
        <span class="folder">{{ status.folder }}</span>
      </span>

      <template v-if="status.branch">
        <span class="divider">|</span>
        <span class="seg branch-seg">
          <MdiIcon :path="mdiSourceBranch" :size="13" class="branch-icon" />
          <span class="branch">{{ status.branch }}</span>
        </span>
      </template>
    </template>

    <template v-else>
      <span class="lbl waiting">waiting for first turn…</span>
    </template>
  </div>
</template>

<style scoped>
.terminal-footer {
  display: flex;
  align-items: center;
  gap: 6px;
  height: 26px;
  padding: 0 8px;
  background: var(--color-bg-subtle);
  border-top: 1px solid var(--color-card-border);
  flex-shrink: 0;
  overflow: hidden;
  font-family: Consolas, 'Cascadia Code', Menlo, 'SF Mono', monospace;
  font-size: 11px;
  user-select: none;
}

.seg {
  display: flex;
  align-items: center;
  gap: 2px;
  white-space: nowrap;
}

.lbl {
  color: var(--color-text-muted);
  opacity: 0.6;
}

.divider {
  color: var(--color-card-border);
  flex-shrink: 0;
}

.spacer { flex: 1; }

.model        { font-weight: 600; color: var(--color-text-primary); }
.model-sonnet { color: #9cdcfe; }
.model-opus   { color: #4ec9b0; }
.model-haiku  { color: #b5cea8; }
.model-flash  { color: #c678dd; }

.tok-in  { color: #4ec9b0; }
.tok-out { color: #c678dd; }
.tok-cw  { color: #569cd6; }
.tok-cr  { color: #d7ba7d; }

.folder-seg { gap: 4px; color: var(--color-text-muted); }
.folder { color: var(--color-text-primary); }

.branch-seg { gap: 3px; }
.branch-icon { color: #F05032; }
.branch { color: #6a9955; font-weight: 600; }

.waiting { font-style: italic; }
</style>
