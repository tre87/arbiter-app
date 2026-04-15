<script setup lang="ts">
import { ref } from 'vue'

type EndMode = 'delete' | 'merge' | 'discard' | 'pr'

const props = defineProps<{
  branchName: string
  mainBranch: string
  mainClaudeRunning: boolean
  onEnd: (mode: EndMode) => Promise<void>
}>()

const emit = defineEmits<{
  (e: 'close'): void
}>()

const ending = ref(false)
const endError = ref('')

async function submit(mode: EndMode) {
  ending.value = true
  endError.value = ''
  try {
    await props.onEnd(mode)
    emit('close')
  } catch (e: any) {
    endError.value = e?.message ?? String(e)
  } finally {
    ending.value = false
  }
}
</script>

<template>
  <Teleport to="body">
    <div class="dialog-overlay" @click.self="emit('close')">
      <div class="dialog">
        <h3>End worktree: {{ branchName }}</h3>
        <p class="dialog-desc">Choose how to handle this worktree:</p>
        <div v-if="endError" class="error">{{ endError }}</div>
        <div class="end-actions">
          <button class="btn-action" :disabled="ending" @click="submit('delete')">
            Delete worktree
            <span class="btn-desc">Remove worktree, keep branch</span>
          </button>
          <button
            class="btn-action"
            :disabled="ending || !mainClaudeRunning"
            :title="!mainClaudeRunning ? 'Start Claude in the main terminal first' : ''"
            @click="submit('merge')"
          >
            Merge &amp; delete
            <span class="btn-desc">Merge into {{ mainBranch }}, then remove</span>
          </button>
          <button
            class="btn-action"
            :disabled="ending || !mainClaudeRunning"
            :title="!mainClaudeRunning ? 'Start Claude in the main terminal first' : ''"
            @click="submit('pr')"
          >
            Create PR &amp; delete
            <span class="btn-desc">Push, create PR, then remove</span>
          </button>
          <button class="btn-action btn-danger" :disabled="ending" @click="submit('discard')">
            Discard changes
            <span class="btn-desc">Force remove, even with uncommitted changes</span>
          </button>
        </div>
        <div class="dialog-actions">
          <button class="btn-secondary" @click="emit('close')">Cancel</button>
        </div>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
.dialog-overlay {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.5);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 1000;
}

.dialog {
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-card-border);
  border-radius: 8px;
  padding: 20px;
  min-width: 340px;
  max-width: 420px;
}

.dialog h3 {
  margin: 0 0 14px;
  font-size: 15px;
  color: var(--color-text-primary);
}

.dialog-desc {
  font-size: 13px;
  color: var(--color-text-secondary);
  margin: 0 0 12px;
}

.error {
  color: var(--color-danger);
  font-size: 12px;
  margin-bottom: 10px;
  padding: 6px 8px;
  background: rgba(239, 68, 68, 0.1);
  border-radius: var(--radius-md);
}

.end-actions {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.dialog-actions {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
  margin-top: 14px;
}

.btn-secondary, .btn-action {
  padding: 6px 14px;
  border-radius: var(--radius-md);
  font-size: 13px;
  cursor: pointer;
  border: 1px solid var(--color-card-border);
}

.btn-secondary {
  background: var(--color-bg-subtle);
  color: var(--color-text-primary);
}

.btn-action {
  background: var(--color-bg-subtle);
  color: var(--color-text-primary);
  text-align: left;
  padding: 8px 12px;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.btn-action:hover {
  background: var(--color-bg-elevated);
  border-color: var(--azure);
}
.btn-action:disabled {
  opacity: 0.5;
  cursor: default;
}

.btn-danger {
  border-color: var(--color-danger);
  color: var(--color-danger);
}
.btn-danger:hover {
  background: rgba(239, 68, 68, 0.1);
}

.btn-desc {
  font-size: 11px;
  color: var(--color-text-muted);
}
</style>
