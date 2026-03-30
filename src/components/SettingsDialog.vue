<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { useDevSettingsStore } from '../stores/devSettings'
import { useUsageStore } from '../stores/usage'

const emit = defineEmits<{ close: [] }>()
const devStore = useDevSettingsStore()
const usageStore = useUsageStore()

const isWindows = navigator.platform.startsWith('Win')
const gitBashAvailable = ref(false)

type Tab = 'general' | 'usage' | 'display'
const activeTab = ref<Tab>('general')

const tabs: { id: Tab; label: string }[] = [
  { id: 'general', label: 'General' },
  { id: 'usage', label: 'Claude Usage' },
  { id: 'display', label: 'Display' },
]

onMounted(async () => {
  if (isWindows) {
    const path = await invoke<string | null>('check_git_bash')
    gitBashAvailable.value = !!path
  }
})

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
      <div class="dialog-sidebar">
        <h3 class="dialog-title">Settings</h3>
        <nav class="tab-nav">
          <button
            v-for="tab in tabs"
            :key="tab.id"
            :class="['tab-btn', { active: activeTab === tab.id }]"
            @click="activeTab = tab.id"
          >
            {{ tab.label }}
          </button>
        </nav>
      </div>

      <div class="dialog-content">
        <!-- General -->
        <div v-if="activeTab === 'general'" class="tab-panel">
          <h4 class="panel-title">Saved Data</h4>
          <div class="panel-body">
            <div class="btn-row">
              <button class="btn btn-secondary" @click="clearSaved('sessions')">Clear saved sessions</button>
              <button class="btn btn-secondary" @click="clearSaved('paths')">Clear saved paths</button>
              <button class="btn btn-secondary" @click="clearSaved('layout')">Clear saved layout</button>
              <button class="btn btn-danger" @click="clearSaved('all')">Clear all saved data</button>
            </div>
          </div>

          <template v-if="isWindows && gitBashAvailable">
            <h4 class="panel-title" style="margin-top: 20px;">Shell</h4>
            <div class="panel-body">
              <label class="toggle-row">
                <span class="toggle-label">Default shell for new terminals</span>
                <select v-model="devStore.defaultShell" class="shell-select">
                  <option value="powershell">PowerShell</option>
                  <option value="gitbash">Git Bash</option>
                </select>
              </label>
            </div>
          </template>

          <h4 class="panel-title" style="margin-top: 20px;">Developer</h4>
          <div class="panel-body">
            <label class="toggle-row">
              <span class="toggle-label">Force peak hours indicator</span>
              <input type="checkbox" v-model="devStore.forcePeakHours" class="toggle-input" />
            </label>
          </div>
        </div>

        <!-- Claude Usage -->
        <div v-if="activeTab === 'usage'" class="tab-panel">
          <h4 class="panel-title">Account</h4>
          <div class="panel-body">
            <template v-if="usageStore.data">
              <div class="account-info">
                <span class="account-detail" v-if="usageStore.data.account_name">{{ usageStore.data.account_name }}</span>
                <span class="account-detail muted" v-if="usageStore.data.account_email">{{ usageStore.data.account_email }}</span>
                <span class="account-detail" v-if="!usageStore.data.account_name && !usageStore.data.account_email">Signed in ({{ usageStore.data.plan }})</span>
              </div>
              <div class="btn-row" style="margin-top: 4px;">
                <button class="btn btn-danger" @click="usageStore.logout()">Sign out</button>
              </div>
            </template>
            <template v-else-if="usageStore.needsLogin">
              <span class="account-detail muted" style="padding: 7px 0;">Not signed in</span>
              <div class="btn-row" style="margin-top: 4px;">
                <button class="btn btn-secondary" @click="usageStore.openLogin()">Sign in</button>
              </div>
            </template>
            <template v-else>
              <span class="account-detail muted" style="padding: 7px 0;">Loading...</span>
            </template>
          </div>

          <h4 class="panel-title" style="margin-top: 20px;">Display</h4>
          <div class="panel-body">
            <label class="toggle-row">
              <span class="toggle-label">Hide usage bar</span>
              <input type="checkbox" v-model="devStore.hideUsageBar" class="toggle-input" />
            </label>
          </div>
        </div>

        <!-- Display -->
        <div v-if="activeTab === 'display'" class="tab-panel">
          <h4 class="panel-title">Display</h4>
          <div class="panel-body">
            <label class="toggle-row">
              <span class="toggle-label">Always show footer bar</span>
              <input type="checkbox" v-model="devStore.alwaysShowFooter" class="toggle-input" />
            </label>
          </div>
        </div>

        <div class="dialog-actions">
          <button class="btn btn-primary" @click="$emit('close')">Close</button>
        </div>
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
  display: flex;
  width: 560px;
  height: 380px;
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
  overflow: hidden;
}

.dialog-sidebar {
  width: 160px;
  flex-shrink: 0;
  background: var(--color-bg);
  border-right: 1px solid var(--color-card-border);
  padding: 20px 12px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.dialog-title {
  margin: 0 0 16px;
  padding: 0 8px;
  font-size: 14px;
  font-weight: 600;
  color: var(--color-text-primary);
}

.tab-nav {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.tab-btn {
  display: block;
  width: 100%;
  padding: 7px 8px;
  background: none;
  border: none;
  color: var(--color-text-secondary);
  font-size: 12px;
  font-family: inherit;
  text-align: left;
  cursor: pointer;
  border-radius: 4px;
  transition: background 0.1s, color 0.1s;
}

.tab-btn:hover {
  background: var(--color-bg-subtle);
  color: var(--color-text-primary);
}

.tab-btn.active {
  background: var(--color-bg-subtle);
  color: var(--color-text-primary);
  font-weight: 500;
}

.dialog-content {
  flex: 1;
  padding: 20px 24px;
  display: flex;
  flex-direction: column;
}

.tab-panel {
  flex: 1;
}

.panel-title {
  margin: 0 0 12px;
  font-size: 13px;
  font-weight: 600;
  color: var(--color-text-primary);
}

.panel-body {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.btn-row {
  display: flex;
  flex-wrap: wrap;
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

.btn-danger {
  background: none;
  color: var(--color-text-secondary);
  border: 1px solid var(--color-card-border);
}

.btn-danger:hover {
  background: rgba(239, 68, 68, 0.15);
  color: var(--color-danger);
  border-color: rgba(239, 68, 68, 0.4);
}

.btn-primary {
  background: var(--color-accent);
  color: #fff;
  border: 1px solid var(--color-accent);
}

.btn-primary:hover {
  background: var(--azure-deep);
  border-color: var(--azure-deep);
}

.account-info {
  padding: 7px 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.account-detail {
  font-size: 12px;
  color: var(--color-text-primary);
}

.account-detail.muted {
  color: var(--color-text-muted);
  font-size: 11px;
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

.shell-select {
  background: var(--color-bg);
  border: 1px solid var(--color-card-border);
  border-radius: 4px;
  color: var(--color-text-primary);
  font-size: 12px;
  font-family: inherit;
  padding: 4px 8px;
  cursor: pointer;
  outline: none;
}

.shell-select:focus {
  border-color: var(--color-accent);
}

.dialog-actions {
  display: flex;
  justify-content: flex-end;
  margin-top: auto;
  padding-top: 16px;
}
</style>
