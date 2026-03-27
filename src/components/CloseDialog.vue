<script setup lang="ts">
import { ref, watch } from 'vue'

const props = defineProps<{
  initialSaveLayout: boolean
  initialSavePaths: boolean
  initialSaveSessions: boolean
}>()

const emit = defineEmits<{
  confirm: [saveLayout: boolean, savePaths: boolean, saveSessions: boolean]
  cancel: []
}>()

const saveLayout = ref(props.initialSaveLayout)
const savePaths = ref(props.initialSavePaths)
const saveSessions = ref(props.initialSaveSessions)

// Enforce checkbox dependencies
watch(saveLayout, (v) => {
  if (!v) {
    savePaths.value = false
    saveSessions.value = false
  }
})
watch(savePaths, (v) => {
  if (!v) saveSessions.value = false
})

function confirm() {
  emit('confirm', saveLayout.value, savePaths.value, saveSessions.value)
}

function cancel() {
  emit('cancel')
}
</script>

<template>
  <div class="dialog-overlay" @mousedown.self="cancel">
    <div class="dialog">
      <h3 class="dialog-title">Save session before closing?</h3>

      <label class="checkbox-row">
        <input type="checkbox" v-model="saveLayout" />
        <span class="checkbox-label">Save layout</span>
        <span class="checkbox-desc">Window size, terminal positions & sizes</span>
      </label>

      <label class="checkbox-row" :class="{ disabled: !saveLayout }">
        <input type="checkbox" v-model="savePaths" :disabled="!saveLayout" />
        <span class="checkbox-label">Save paths</span>
        <span class="checkbox-desc">Working directory of each terminal</span>
      </label>

      <label class="checkbox-row" :class="{ disabled: !savePaths }">
        <input type="checkbox" v-model="saveSessions" :disabled="!savePaths" />
        <span class="checkbox-label">Save sessions</span>
        <span class="checkbox-desc">Resume Claude sessions on next launch</span>
      </label>

      <div class="dialog-actions">
        <button class="btn btn-secondary" @click="cancel">Cancel</button>
        <button class="btn btn-primary" @click="confirm">Close</button>
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
  min-width: 340px;
  max-width: 420px;
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
}

.dialog-title {
  margin: 0 0 16px;
  font-size: 14px;
  font-weight: 600;
  color: var(--color-text-primary);
}

.checkbox-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  padding: 8px 0;
  cursor: pointer;
  user-select: none;
}

.checkbox-row.disabled {
  opacity: 0.4;
  cursor: default;
}

.checkbox-row input[type="checkbox"] {
  accent-color: var(--color-accent);
  width: 14px;
  height: 14px;
  cursor: inherit;
}

.checkbox-label {
  font-size: 13px;
  font-weight: 500;
  color: var(--color-text-primary);
}

.checkbox-desc {
  width: 100%;
  padding-left: 22px;
  font-size: 11px;
  color: var(--color-text-muted);
  margin-top: -4px;
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
</style>
