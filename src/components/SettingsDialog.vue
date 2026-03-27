<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core'
import { useDevSettingsStore } from '../stores/devSettings'

const emit = defineEmits<{ close: [] }>()
const devStore = useDevSettingsStore()

async function clearSaved(what: 'all' | 'layout' | 'paths' | 'sessions') {
  try {
    const current = await invoke<Record<string, any> | null>('load_config')
    const config: Record<string, any> = {
      closeOptions: current?.closeOptions,
    }

    if (what === 'all' || what === 'layout') {
      // Keep only closeOptions
    } else if (what === 'paths') {
      config.layout = current?.layout
      config.window = current?.window
      config.terminals = current?.terminals?.map((t: any) => ({ name: t.name }))
    } else if (what === 'sessions') {
      config.layout = current?.layout
      config.window = current?.window
      config.terminals = current?.terminals?.map((t: any) => ({ name: t.name, cwd: t.cwd }))
    }

    await invoke('save_config', { config })
  } catch { /* ignore */ }
}
</script>

<template>
  <div class="dialog-overlay" @mousedown.self="$emit('close')">
    <div class="dialog">
      <h3 class="dialog-title">Settings</h3>

      <div class="section">
        <h4 class="section-title">Saved Data</h4>
        <div class="section-body">
          <button class="menu-item" @click="clearSaved('sessions')">Clear saved sessions</button>
          <button class="menu-item" @click="clearSaved('paths')">Clear saved paths</button>
          <button class="menu-item" @click="clearSaved('layout')">Clear saved layout</button>
          <button class="menu-item menu-item-danger" @click="clearSaved('all')">Clear all</button>
        </div>
      </div>

      <div class="section">
        <h4 class="section-title">Developer</h4>
        <div class="section-body">
          <label class="toggle-row">
            <span class="toggle-label">Force peak hours indicator</span>
            <input type="checkbox" v-model="devStore.forcePeakHours" class="toggle-input" />
          </label>
        </div>
      </div>

      <div class="dialog-actions">
        <button class="btn btn-primary" @click="$emit('close')">Close</button>
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
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
}

.dialog-title {
  margin: 0 0 16px;
  font-size: 14px;
  font-weight: 600;
  color: var(--color-text-primary);
}

.section {
  margin-bottom: 16px;
}

.section-title {
  margin: 0 0 8px;
  font-size: 11px;
  font-weight: 600;
  color: var(--color-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.06em;
}

.section-body {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.menu-item {
  display: block;
  width: 100%;
  padding: 7px 10px;
  background: none;
  border: none;
  color: var(--color-text-secondary);
  font-size: 12px;
  text-align: left;
  cursor: pointer;
  border-radius: 4px;
  transition: background 0.1s, color 0.1s;
}

.menu-item:hover {
  background: var(--color-bg-subtle);
  color: var(--color-text-primary);
}

.menu-item-danger:hover {
  background: rgba(239, 68, 68, 0.15);
  color: var(--color-danger);
}

.toggle-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 7px 10px;
  border-radius: 4px;
  cursor: pointer;
  transition: background 0.1s;
}

.toggle-row:hover {
  background: var(--color-bg-subtle);
}

.toggle-label {
  font-size: 12px;
  color: var(--color-text-secondary);
}

.toggle-input {
  accent-color: var(--color-accent);
  cursor: pointer;
}

.dialog-actions {
  display: flex;
  justify-content: flex-end;
  margin-top: 16px;
}

.btn-primary {
  padding: 6px 16px;
  border-radius: 4px;
  font-size: 12px;
  font-weight: 500;
  cursor: pointer;
  background: var(--color-accent);
  color: #fff;
  border: 1px solid var(--color-accent);
  transition: background 0.15s, border-color 0.15s;
}

.btn-primary:hover {
  background: var(--azure-deep);
  border-color: var(--azure-deep);
}
</style>
