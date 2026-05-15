<script setup lang="ts">
import { ref, computed } from 'vue'
import { useUsageStore, type OrgInfo } from '../stores/usage'

const store = useUsageStore()

// When the dialog is forced open by needsOrgSelection, hide Cancel — the user
// must pick one to make the stats bar functional. Manual opens (from Settings)
// can be cancelled.
const dismissible = computed(() => !store.needsOrgSelection)

const currentUuid = computed(() => store.availableOrgs.find(o => o.name === store.data?.org_name)?.uuid ?? null)
const selectedUuid = ref<string | null>(currentUuid.value)

// Keep selection in sync if list reloads
function pick(org: OrgInfo) {
  selectedUuid.value = org.uuid
}

const submitting = ref(false)
async function confirm() {
  const org = store.availableOrgs.find(o => o.uuid === selectedUuid.value)
  if (!org) return
  submitting.value = true
  try {
    await store.setSelectedOrg(org)
  } finally {
    submitting.value = false
  }
}

function cancel() {
  if (!dismissible.value) return
  store.closeOrgPicker()
}
</script>

<template>
  <div class="dialog-overlay" @mousedown.self="cancel">
    <div class="dialog">
      <h3 class="dialog-title">Select organization</h3>
      <p class="dialog-subtitle">
        Your Claude account has multiple organizations. Pick the one whose usage stats you want to track.
      </p>

      <div class="org-list">
        <button
          v-for="org in store.availableOrgs"
          :key="org.uuid"
          :class="['org-row', { selected: selectedUuid === org.uuid }]"
          @click="pick(org)"
        >
          <span class="org-name">{{ org.name }}</span>
          <span v-if="currentUuid === org.uuid" class="org-current">current</span>
        </button>
        <div v-if="store.availableOrgs.length === 0" class="org-empty">
          No organizations available. Try signing out and back in.
        </div>
      </div>

      <div class="dialog-actions">
        <button v-if="dismissible" class="btn btn-secondary" @click="cancel">Cancel</button>
        <button
          class="btn btn-primary"
          :disabled="!selectedUuid || submitting"
          @click="confirm"
        >
          {{ submitting ? 'Loading…' : 'Use this organization' }}
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
  width: 420px;
  padding: 20px 24px;
  display: flex;
  flex-direction: column;
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
}

.dialog-title {
  margin: 0 0 6px;
  font-size: 14px;
  font-weight: 600;
  color: var(--color-text-primary);
}

.dialog-subtitle {
  margin: 0 0 16px;
  font-size: 12px;
  color: var(--color-text-muted);
  line-height: 1.4;
}

.org-list {
  display: flex;
  flex-direction: column;
  gap: 4px;
  max-height: 280px;
  overflow-y: auto;
  margin-bottom: 16px;
}

.org-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 10px 12px;
  background: var(--color-bg-subtle);
  border: 1px solid var(--color-card-border);
  border-radius: 4px;
  cursor: pointer;
  font-family: inherit;
  text-align: left;
  transition: border-color 0.1s, background 0.1s;
}

.org-row:hover {
  border-color: var(--color-text-muted);
}

.org-row.selected {
  border-color: var(--color-accent);
  background: var(--color-bg);
}

.org-name {
  font-size: 12px;
  color: var(--color-text-primary);
}

.org-current {
  font-size: 10px;
  color: var(--color-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.08em;
}

.org-empty {
  padding: 16px;
  text-align: center;
  font-size: 12px;
  color: var(--color-text-muted);
}

.dialog-actions {
  display: flex;
  justify-content: flex-end;
  gap: 6px;
}

.btn {
  padding: 6px 14px;
  border-radius: 4px;
  font-size: 12px;
  font-weight: 500;
  font-family: inherit;
  cursor: pointer;
  transition: background 0.15s, border-color 0.15s, color 0.15s;
}

.btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.btn-secondary {
  background: var(--color-bg-subtle);
  color: var(--color-text-secondary);
  border: 1px solid var(--color-card-border);
}

.btn-secondary:hover {
  background: var(--color-bg);
  color: var(--color-text-primary);
  border-color: var(--color-text-muted);
}

.btn-primary {
  background: var(--color-accent);
  color: #fff;
  border: 1px solid var(--color-accent);
}

.btn-primary:hover:not(:disabled) {
  background: var(--azure-deep);
  border-color: var(--azure-deep);
}
</style>
