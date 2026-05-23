<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { getVersion } from '@tauri-apps/api/app'
import { open } from '@tauri-apps/plugin-dialog'
import { useDevSettingsStore } from '../stores/devSettings'
import { useUsageStore } from '../stores/usage'
import { useFilesSettingsStore } from '../stores/filesSettings'

const emit = defineEmits<{ close: [] }>()
const devStore = useDevSettingsStore()
const usageStore = useUsageStore()
const filesStore = useFilesSettingsStore()

const isWindows = navigator.platform.startsWith('Win')
const gitBashAvailable = ref(false)
const appVersion = ref('')
const screenshotDefaultPath = ref('')

type Tab = 'general' | 'files' | 'usage' | 'display'
const activeTab = ref<Tab>('general')

const tabs: { id: Tab; label: string }[] = [
  { id: 'general', label: 'General' },
  { id: 'files', label: 'Files' },
  { id: 'usage', label: 'Claude Usage' },
  { id: 'display', label: 'Display' },
]

onMounted(async () => {
  if (isWindows) {
    const path = await invoke<string | null>('check_git_bash')
    gitBashAvailable.value = !!path
  }
  try {
    appVersion.value = await getVersion()
  } catch { /* ignore */ }
  try {
    screenshotDefaultPath.value = await filesStore.getScreenshotDefaultDir()
  } catch { /* ignore — placeholder stays empty */ }
})

async function browseScreenshotFolder() {
  try {
    const selected = await open({
      directory: true,
      defaultPath: filesStore.screenshotFolder || screenshotDefaultPath.value || undefined,
      title: 'Select Screenshot Folder',
    })
    if (typeof selected === 'string') filesStore.setScreenshotFolder(selected)
  } catch (e) {
    console.error('Arbiter: browseScreenshotFolder failed:', e)
  }
}

function resetScreenshotFolder() {
  filesStore.setScreenshotFolder(null)
}

async function clearSaved(what: 'all' | 'layout' | 'paths' | 'sessions') {
  try {
    const current = await invoke<Record<string, any> | null>('load_config')
    let config: Record<string, any> = {}
    switch (what) {
      // Both 'all' and 'layout' wipe everything (layout-only retention would
      // strip terminal identity, which the rest of restore depends on).
      case 'all':
      case 'layout':
        break
      case 'paths':
        config = {
          layout: current?.layout,
          window: current?.window,
          terminals: current?.terminals?.map((t: any) => ({ name: t.name })),
        }
        break
      case 'sessions':
        config = {
          layout: current?.layout,
          window: current?.window,
          terminals: current?.terminals?.map((t: any) => ({ name: t.name, cwd: t.cwd })),
        }
        break
    }
    await invoke('save_config', { config })
  } catch (e) {
    console.error('Arbiter: clearSaved failed:', e)
  }
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
        <div v-if="appVersion" class="sidebar-version">v{{ appVersion }}</div>
      </div>

      <div class="dialog-content">
        <!-- General -->
        <div v-if="activeTab === 'general'" class="tab-panel">
          <div class="panel-section">
            <h4 class="panel-title">Saved Data</h4>
            <div class="panel-body">
              <div class="btn-row">
                <button class="btn btn-secondary" @click="clearSaved('sessions')">Clear saved sessions</button>
                <button class="btn btn-secondary" @click="clearSaved('paths')">Clear saved paths</button>
                <button class="btn btn-secondary" @click="clearSaved('layout')">Clear saved layout</button>
                <button class="btn btn-danger" @click="clearSaved('all')">Clear all saved data</button>
              </div>
            </div>
          </div>

          <div v-if="isWindows && gitBashAvailable" class="panel-section">
            <h4 class="panel-title">Shell</h4>
            <div class="panel-body">
              <label class="toggle-row">
                <span class="toggle-label">Default shell for new terminals</span>
                <select v-model="devStore.defaultShell" class="shell-select">
                  <option value="powershell">PowerShell</option>
                  <option value="gitbash">Git Bash</option>
                </select>
              </label>
            </div>
          </div>

        </div>

        <!-- Files -->
        <div v-if="activeTab === 'files'" class="tab-panel">
          <div class="panel-section">
            <h4 class="panel-title">Screenshot Folder</h4>
            <div class="panel-body">
              <p class="panel-hint">
                Folder opened by <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>S</kbd>. Leave blank to use the system default.
              </p>
              <div class="path-row">
                <input
                  type="text"
                  class="path-input"
                  :value="filesStore.screenshotFolder ?? ''"
                  :placeholder="screenshotDefaultPath || 'System default'"
                  @input="filesStore.setScreenshotFolder(($event.target as HTMLInputElement).value)"
                />
                <button class="btn btn-secondary" @click="browseScreenshotFolder">Browse…</button>
                <button
                  v-if="filesStore.screenshotFolder"
                  class="btn btn-secondary"
                  @click="resetScreenshotFolder"
                  title="Reset to system default"
                >Reset</button>
              </div>
            </div>
          </div>
        </div>

        <!-- Claude Usage -->
        <div v-if="activeTab === 'usage'" class="tab-panel">
          <div class="panel-section">
            <h4 class="panel-title">Account</h4>
            <div class="panel-body">
              <template v-if="usageStore.data">
                <div class="account-info">
                  <span v-if="usageStore.data.account_name" class="account-name">{{ usageStore.data.account_name }}</span>
                  <span v-if="usageStore.data.account_email" class="account-email">{{ usageStore.data.account_email }}</span>
                  <span v-if="!usageStore.data.account_name && !usageStore.data.account_email" class="account-name">Signed in</span>
                  <div v-if="usageStore.data.plan" class="account-meta-row">
                    <span class="meta-label">Plan</span>
                    <span class="meta-value">{{ usageStore.data.plan }}</span>
                  </div>
                  <div v-if="usageStore.data.org_name" class="account-meta-row">
                    <span class="meta-label">Organization</span>
                    <span class="meta-value">{{ usageStore.data.org_name }}</span>
                  </div>
                </div>
                <div class="btn-row">
                  <button v-if="usageStore.data.has_multiple_orgs" class="btn btn-secondary" @click="usageStore.openOrgPicker()">Switch organization</button>
                  <button class="btn btn-danger" @click="usageStore.logout()">Sign out</button>
                </div>
              </template>
              <template v-else-if="usageStore.needsLogin">
                <span class="account-empty">Not signed in</span>
                <div class="btn-row">
                  <button class="btn btn-secondary" @click="usageStore.openLogin()">Sign in</button>
                </div>
              </template>
              <template v-else>
                <span class="account-empty">Loading...</span>
              </template>
            </div>
          </div>

          <div class="panel-section">
            <h4 class="panel-title">Display</h4>
            <div class="panel-body">
              <label class="toggle-row">
                <span class="toggle-label">Hide usage bar</span>
                <span class="switch">
                  <input type="checkbox" v-model="devStore.hideUsageBar" />
                  <span class="switch-track"></span>
                </span>
              </label>
            </div>
          </div>
        </div>

        <!-- Display -->
        <div v-if="activeTab === 'display'" class="tab-panel">
          <div class="panel-section">
            <h4 class="panel-title">Display</h4>
            <div class="panel-body">
              <label class="toggle-row">
                <span class="toggle-label">Always show footer bar</span>
                <span class="switch">
                  <input type="checkbox" v-model="devStore.alwaysShowFooter" />
                  <span class="switch-track"></span>
                </span>
              </label>
              <label class="toggle-row">
                <span class="toggle-label">Overview: only show terminals running Claude</span>
                <span class="switch">
                  <input type="checkbox" v-model="devStore.overviewClaudeOnly" />
                  <span class="switch-track"></span>
                </span>
              </label>
            </div>
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
  width: 640px;
  height: 440px;
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
  overflow: hidden;
}

.dialog-sidebar {
  width: 184px;
  flex-shrink: 0;
  background: var(--color-bg);
  border-right: 1px solid var(--color-card-border);
  padding: 24px 14px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.dialog-title {
  margin: 0 0 18px;
  padding: 0 10px;
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--color-text-muted);
}

.tab-nav {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.tab-btn {
  display: block;
  width: 100%;
  padding: 9px 12px;
  background: none;
  border: none;
  border-left: 3px solid transparent;
  color: var(--color-text-secondary);
  font-size: 13px;
  font-family: inherit;
  text-align: left;
  cursor: pointer;
  border-radius: var(--radius-sm);
  transition: background 0.12s, color 0.12s, border-color 0.12s;
}

.tab-btn:hover {
  background: var(--color-bg-subtle);
  color: var(--color-text-primary);
}

.tab-btn.active {
  background: var(--color-bg-subtle);
  color: var(--color-text-primary);
  font-weight: 500;
  border-left-color: var(--color-accent);
}

.sidebar-version {
  margin-top: auto;
  padding: 0 10px;
  font-size: 11px;
  color: var(--color-text-muted);
  letter-spacing: 0.04em;
}

.dialog-content {
  flex: 1;
  padding: 24px 28px;
  display: flex;
  flex-direction: column;
  min-width: 0;
}

.tab-panel {
  flex: 1;
  overflow-y: auto;
}

.panel-section + .panel-section {
  margin-top: 22px;
}

.panel-title {
  margin: 0 0 10px;
  padding-bottom: 6px;
  border-bottom: 1px solid var(--color-card-border);
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--color-text-muted);
}

.panel-body {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.btn-row {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.panel-hint {
  margin: 0 0 10px;
  padding: 0 4px;
  font-size: 12px;
  color: var(--color-text-muted);
  line-height: 1.5;
}

.panel-hint kbd {
  display: inline-block;
  font-family: inherit;
  font-size: 10px;
  padding: 1px 5px;
  background: var(--color-bg-subtle);
  border: 1px solid var(--color-card-border);
  border-radius: 3px;
  color: var(--color-text-secondary);
}

.path-row {
  display: flex;
  gap: 8px;
  align-items: center;
}

.path-input {
  flex: 1;
  min-width: 0;
  padding: 7px 10px;
  background: var(--color-bg);
  border: 1px solid var(--color-card-border);
  border-radius: var(--radius-md);
  color: var(--color-text-primary);
  font-size: 12px;
  font-family: inherit;
  outline: none;
  transition: border-color 0.12s, box-shadow 0.12s;
}

.path-input::placeholder {
  color: var(--color-text-muted);
}

.path-input:focus {
  border-color: var(--color-accent);
  box-shadow: 0 0 0 2px rgba(51, 153, 255, 0.25);
}

.btn {
  padding: 8px 16px;
  border-radius: var(--radius-md);
  font-size: 13px;
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
  border-color: var(--color-card-border-hover);
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
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 4px 0 8px;
}

.account-name {
  font-size: 13px;
  font-weight: 500;
  color: var(--color-text-primary);
}

.account-email {
  font-size: 12px;
  color: var(--color-text-muted);
}

.account-meta-row {
  display: flex;
  gap: 8px;
  font-size: 12px;
  margin-top: 2px;
}

.meta-label {
  color: var(--color-text-muted);
  min-width: 92px;
}

.meta-value {
  color: var(--color-text-secondary);
}

.account-empty {
  font-size: 13px;
  color: var(--color-text-muted);
  padding: 4px 0;
}

.toggle-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 10px 12px;
  min-height: 36px;
  border-radius: var(--radius-sm);
  cursor: pointer;
  transition: background 0.12s;
}

.toggle-row:hover {
  background: var(--color-bg-subtle);
}

.toggle-label {
  font-size: 13px;
  color: var(--color-text-secondary);
}

/* iOS-style toggle switch */
.switch {
  position: relative;
  display: inline-block;
  width: 36px;
  height: 20px;
  flex-shrink: 0;
}

.switch input[type="checkbox"] {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  margin: 0;
  opacity: 0;
  cursor: pointer;
  z-index: 1;
}

.switch-track {
  position: absolute;
  inset: 0;
  background: var(--color-card-border);
  border-radius: 999px;
  transition: background 0.18s, box-shadow 0.12s;
}

.switch-track::after {
  content: '';
  position: absolute;
  left: 2px;
  top: 2px;
  width: 16px;
  height: 16px;
  background: #fff;
  border-radius: 50%;
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.4);
  transition: transform 0.18s;
}

.switch input[type="checkbox"]:checked + .switch-track {
  background: var(--color-accent);
}

.switch input[type="checkbox"]:checked + .switch-track::after {
  transform: translateX(16px);
}

.switch input[type="checkbox"]:focus-visible + .switch-track {
  box-shadow: 0 0 0 2px rgba(51, 153, 255, 0.35);
}

/* Custom select with chevron */
.shell-select {
  background-color: var(--color-bg);
  background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='10' height='6' viewBox='0 0 10 6'%3E%3Cpath d='M1 1l4 4 4-4' stroke='%23a0aab8' stroke-width='1.5' fill='none' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E");
  background-repeat: no-repeat;
  background-position: right 10px center;
  border: 1px solid var(--color-card-border);
  border-radius: var(--radius-md);
  color: var(--color-text-primary);
  font-size: 13px;
  font-family: inherit;
  padding: 7px 30px 7px 12px;
  min-width: 140px;
  cursor: pointer;
  outline: none;
  appearance: none;
  -webkit-appearance: none;
  transition: border-color 0.12s, box-shadow 0.12s;
}

.shell-select:hover {
  border-color: var(--color-card-border-hover);
}

.shell-select:focus {
  border-color: var(--color-accent);
  box-shadow: 0 0 0 2px rgba(51, 153, 255, 0.25);
}

.shell-select option {
  background: var(--color-bg-elevated);
  color: var(--color-text-primary);
}

.dialog-actions {
  display: flex;
  justify-content: flex-end;
  margin-top: auto;
  padding-top: 18px;
  border-top: 1px solid var(--color-card-border);
}
</style>
