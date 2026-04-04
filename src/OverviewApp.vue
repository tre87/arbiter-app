<script setup lang="ts">
import { ref, onMounted, onBeforeUnmount, computed } from 'vue'
import { listen, emit, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import PulseLoader from './components/PulseLoader.vue'

interface TerminalInfo {
  paneId: string
  workspaceIndex: number
  workspaceName: string
  name: string
  status: 'idle' | 'running' | 'working'
}

const terminals = ref<TerminalInfo[]>([])
const error = ref('')
let unlistenUpdate: UnlistenFn | null = null

const grouped = computed(() => {
  const groups: Array<{
    workspaceName: string
    workspaceIndex: number
    terminals: TerminalInfo[]
  }> = []

  let currentGroup: typeof groups[number] | null = null
  for (const t of terminals.value) {
    if (!currentGroup || currentGroup.workspaceIndex !== t.workspaceIndex) {
      currentGroup = { workspaceName: t.workspaceName, workspaceIndex: t.workspaceIndex, terminals: [] }
      groups.push(currentGroup)
    }
    currentGroup.terminals.push(t)
  }
  return groups
})

function handleClick(workspaceIndex: number, paneId: string) {
  emit('overview-navigate', { workspaceIndex, paneId })
}

async function hideWindow() {
  await emit('overview-closed')
  getCurrentWindow().hide()
}

onMounted(async () => {
  try {
    unlistenUpdate = await listen<TerminalInfo[]>('overview-update', (event) => {
      terminals.value = event.payload
    })

    // Request initial state from main window
    await emit('overview-request-update')
  } catch (e: any) {
    error.value = String(e)
  }

  window.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') hideWindow()
  })
})

onBeforeUnmount(() => {
  unlistenUpdate?.()
})
</script>

<template>
  <div class="overview">
    <div class="overview-titlebar" data-tauri-drag-region>
      <span class="overview-title" data-tauri-drag-region>Arbiter</span>
      <button class="close-btn" @click="hideWindow">×</button>
    </div>
    <div class="overview-content">
      <div v-if="error" class="overview-empty" style="color: #ef4444">{{ error }}</div>
      <div v-else-if="grouped.length === 0" class="overview-empty">No terminals</div>
      <div v-for="group in grouped" :key="group.workspaceIndex" class="overview-group">
        <div class="overview-ws-header">{{ group.workspaceName }}</div>
        <div
          v-for="t in group.terminals"
          :key="t.paneId"
          class="overview-row"
          @click="handleClick(group.workspaceIndex, t.paneId)"
        >
          <span class="overview-name">{{ t.name }}</span>
          <span class="overview-status">
            <span v-if="t.status === 'idle'" class="status-dot idle" />
            <span v-else-if="t.status === 'running'" class="status-dot running" />
            <PulseLoader v-else size="3px" gap="3px" />
          </span>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.overview {
  height: 100vh;
  display: flex;
  flex-direction: column;
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-card-border);
  border-radius: 8px;
  overflow: hidden;
  user-select: none;
}

.overview-titlebar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  height: 32px;
  padding: 0 8px 0 12px;
  background: var(--color-bg);
  border-bottom: 1px solid var(--color-card-border);
  flex-shrink: 0;
}

.overview-title {
  font-size: 11px;
  font-weight: 600;
  color: var(--color-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.08em;
}

.close-btn {
  background: none;
  border: none;
  color: var(--color-text-muted);
  font-size: 16px;
  cursor: pointer;
  padding: 0 4px;
  line-height: 1;
  border-radius: 3px;
  transition: color 0.1s, background 0.1s;
}

.close-btn:hover {
  color: var(--color-text-primary);
  background: var(--color-card-border);
}

.overview-content {
  flex: 1;
  overflow-y: auto;
  padding: 4px 0;
}

.overview-empty {
  padding: 16px;
  text-align: center;
  font-size: 11px;
  color: var(--color-text-muted);
}

.overview-group + .overview-group {
  border-top: 1px solid var(--color-card-border);
  margin-top: 2px;
  padding-top: 2px;
}

.overview-ws-header {
  padding: 4px 12px 2px;
  font-size: 10px;
  font-weight: 600;
  color: var(--color-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.06em;
}

.overview-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 5px 12px;
  cursor: pointer;
  transition: background 0.1s;
}

.overview-row:hover {
  background: var(--color-card-border);
}

.overview-name {
  font-size: 12px;
  color: var(--color-text-primary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.overview-status {
  display: flex;
  align-items: center;
  margin-left: 12px;
  flex-shrink: 0;
}

.status-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
}

.status-dot.idle {
  background: var(--color-text-muted);
  opacity: 0.5;
}

.status-dot.running {
  background: var(--color-success);
  animation: pulse-running 1.5s ease-in-out infinite;
}

@keyframes pulse-running {
  0%, 100% { opacity: 0.5; transform: scale(0.9); }
  50% { opacity: 1; transform: scale(1.1); }
}
</style>
