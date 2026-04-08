<script setup lang="ts">
import { onMounted, onBeforeUnmount, watch, nextTick } from 'vue'
import { useConfirm } from '../composables/useConfirm'

const { pending, resolve } = useConfirm()

function onKeyDown(e: KeyboardEvent) {
  if (!pending.value) return
  if (e.key === 'Escape') { e.preventDefault(); resolve(false) }
  else if (e.key === 'Enter') { e.preventDefault(); resolve(true) }
}

onMounted(() => window.addEventListener('keydown', onKeyDown, { capture: true }))
onBeforeUnmount(() => window.removeEventListener('keydown', onKeyDown, { capture: true }))

// Auto-focus the confirm button when the dialog opens
watch(pending, (v) => {
  if (v) {
    nextTick(() => {
      const btn = document.querySelector('.confirm-dialog .btn-primary') as HTMLButtonElement | null
      btn?.focus()
    })
  }
})
</script>

<template>
  <div v-if="pending" class="dialog-overlay" @mousedown.self="resolve(false)">
    <div class="dialog confirm-dialog">
      <h3 class="dialog-title">{{ pending.title }}</h3>
      <p v-if="pending.message" class="dialog-message">{{ pending.message }}</p>
      <div class="dialog-actions">
        <button class="btn btn-secondary" @click="resolve(false)">
          {{ pending.cancelText ?? 'Cancel' }}
        </button>
        <button
          class="btn btn-primary"
          :class="{ danger: pending.danger }"
          @click="resolve(true)"
        >
          {{ pending.confirmText ?? 'Confirm' }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.dialog-overlay {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.6);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 9999;
}

.dialog {
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-card-border);
  border-radius: 8px;
  padding: 20px 24px;
  min-width: 320px;
  max-width: 420px;
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
}

.dialog-title {
  margin: 0 0 8px;
  font-size: 14px;
  font-weight: 600;
  color: var(--color-text-primary);
}

.dialog-message {
  margin: 0 0 4px;
  font-size: 12px;
  color: var(--color-text-secondary);
  line-height: 1.5;
}

.dialog-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 20px;
}

.btn {
  padding: 6px 16px;
  border-radius: 4px;
  font-size: 12px;
  font-weight: 500;
  cursor: pointer;
  border: 1px solid var(--color-card-border);
  font-family: inherit;
  transition: background 0.15s, border-color 0.15s;
}

.btn-secondary {
  background: transparent;
  color: var(--color-text-secondary);
}

.btn-secondary:hover {
  background: var(--color-bg-subtle);
  color: var(--color-text-primary);
}

.btn-primary {
  background: var(--color-accent);
  color: #fff;
  border-color: var(--color-accent);
}

.btn-primary:hover {
  background: var(--azure-deep);
  border-color: var(--azure-deep);
}

.btn-primary.danger {
  background: var(--color-danger, #e81123);
  border-color: var(--color-danger, #e81123);
}

.btn-primary.danger:hover {
  background: #c40f1f;
  border-color: #c40f1f;
}
</style>
