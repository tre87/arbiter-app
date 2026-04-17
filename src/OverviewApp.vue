<script setup lang="ts">
import { ref, onMounted, onBeforeUnmount, computed } from 'vue'
import { listen, emit, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import PulseLoader from './components/PulseLoader.vue'
import MdiIcon from './components/MdiIcon.vue'
import { mdiConsole, mdiFolder, mdiChevronRight, mdiChevronDown } from '@mdi/js'

interface TerminalInfo {
  paneId: string
  workspaceId: string
  workspaceIndex: number
  workspaceName: string
  workspaceType: 'terminal' | 'project'
  name: string
  status: 'idle' | 'running' | 'ready' | 'working' | 'attention'
}

interface WorkspaceGroup {
  workspaceId: string
  workspaceName: string
  workspaceIndex: number
  workspaceType: 'terminal' | 'project'
  terminals: TerminalInfo[]
}

const terminals = ref<TerminalInfo[]>([])
const error = ref('')
const collapsed = ref<Set<string>>(new Set())
let unlistenUpdate: UnlistenFn | null = null

const grouped = computed<WorkspaceGroup[]>(() => {
  const groups: WorkspaceGroup[] = []
  let currentGroup: WorkspaceGroup | null = null
  for (const t of terminals.value) {
    if (!currentGroup || currentGroup.workspaceIndex !== t.workspaceIndex) {
      currentGroup = {
        workspaceId: t.workspaceId,
        workspaceName: t.workspaceName,
        workspaceIndex: t.workspaceIndex,
        workspaceType: t.workspaceType,
        terminals: [],
      }
      groups.push(currentGroup)
    }
    currentGroup.terminals.push(t)
  }
  return groups
})

function handleClick(workspaceIndex: number, paneId: string) {
  emit('overview-navigate', { workspaceIndex, paneId })
}

function toggleCollapsed(id: string) {
  const next = new Set(collapsed.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  collapsed.value = next
}

async function hideWindow() {
  await emit('overview-closed')
  getCurrentWindow().hide()
}

function handleContextMenu(e: MouseEvent) {
  e.preventDefault()
}

// ── Drag reorder (pointer-based) ───────────────────────────────────────────
const dragIndex = ref<number | null>(null)
const dropIndex = ref<number | null>(null)
const contentEl = ref<HTMLElement | null>(null)
const DRAG_THRESHOLD = 4

function onHeaderPointerDown(e: PointerEvent, index: number) {
  if (e.button !== 0) return
  const startY = e.clientY
  const pointerId = e.pointerId
  let started = false

  const onMove = (ev: PointerEvent) => {
    if (ev.pointerId !== pointerId) return
    if (!started && Math.abs(ev.clientY - startY) >= DRAG_THRESHOLD) {
      started = true
      dragIndex.value = index
    }
    if (!started || !contentEl.value) return
    // Hit-test workspace groups by Y-center to pick insertion point
    const headers = Array.from(contentEl.value.querySelectorAll('.overview-group')) as HTMLElement[]
    let target = headers.length // default: insert at end
    for (let i = 0; i < headers.length; i++) {
      const rect = headers[i].getBoundingClientRect()
      if (ev.clientY < rect.top + rect.height / 2) { target = i; break }
    }
    // Clamp: moving from → target (accounting for self-removal)
    if (target > dragIndex.value!) target--
    if (target < 0) target = 0
    if (target >= grouped.value.length) target = grouped.value.length - 1
    dropIndex.value = target
  }

  const onUp = (ev: PointerEvent) => {
    if (ev.pointerId !== pointerId) return
    document.removeEventListener('pointermove', onMove)
    document.removeEventListener('pointerup', onUp)
    if (started && dragIndex.value != null && dropIndex.value != null && dragIndex.value !== dropIndex.value) {
      emit('overview-reorder-workspace', { from: dragIndex.value, to: dropIndex.value })
    }
    // Swallow the click that follows a drag so it doesn't toggle collapse
    if (started) {
      const swallow = (ce: MouseEvent) => {
        ce.stopPropagation()
        ce.preventDefault()
        window.removeEventListener('click', swallow, true)
      }
      window.addEventListener('click', swallow, true)
    }
    dragIndex.value = null
    dropIndex.value = null
    started = false
  }

  document.addEventListener('pointermove', onMove)
  document.addEventListener('pointerup', onUp)
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
  window.addEventListener('contextmenu', handleContextMenu)
})

onBeforeUnmount(() => {
  unlistenUpdate?.()
  window.removeEventListener('contextmenu', handleContextMenu)
})
</script>

<template>
  <div class="overview">
    <div class="overview-titlebar" data-tauri-drag-region>
      <span class="overview-title" data-tauri-drag-region>Arbiter</span>
      <button class="close-btn" @click="hideWindow">×</button>
    </div>
    <div ref="contentEl" class="overview-content">
      <div v-if="error" class="overview-empty" style="color: #ef4444">{{ error }}</div>
      <div v-else-if="grouped.length === 0" class="overview-empty">No terminals</div>
      <div
        v-for="(group, gi) in grouped"
        :key="group.workspaceId"
        class="overview-group"
        :class="{
          collapsed: collapsed.has(group.workspaceId),
          dragging: dragIndex === gi,
          'drop-target': dropIndex === gi && dragIndex !== gi,
          'drop-above': dropIndex === gi && dragIndex !== null && dragIndex > gi,
          'drop-below': dropIndex === gi && dragIndex !== null && dragIndex < gi,
        }"
      >
        <div
          class="overview-ws-header"
          @click="toggleCollapsed(group.workspaceId)"
          @pointerdown="onHeaderPointerDown($event, gi)"
        >
          <MdiIcon
            :path="collapsed.has(group.workspaceId) ? mdiChevronRight : mdiChevronDown"
            :size="12"
            class="ws-chevron"
          />
          <MdiIcon :path="group.workspaceType === 'project' ? mdiFolder : mdiConsole" :size="12" />
          <span class="ws-name">{{ group.workspaceName }}</span>
          <span class="ws-count">{{ group.terminals.length }}</span>
        </div>
        <div v-show="!collapsed.has(group.workspaceId)" class="overview-rows">
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
              <span v-else-if="t.status === 'ready'" class="status-dot ready" />
              <span v-else-if="t.status === 'attention'" class="status-dot attention" />
              <PulseLoader v-else size="3px" gap="3px" />
            </span>
          </div>
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

.overview-group.dragging {
  opacity: 0.4;
}

.overview-group.drop-above {
  box-shadow: inset 0 2px 0 0 var(--color-accent);
}

.overview-group.drop-below {
  box-shadow: inset 0 -2px 0 0 var(--color-accent);
}

.overview-ws-header {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 4px 12px 2px;
  font-size: 10px;
  font-weight: 600;
  color: var(--color-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.06em;
  cursor: pointer;
  user-select: none;
  touch-action: none;
  transition: background 0.1s, color 0.1s;
}

.overview-ws-header:hover {
  color: var(--color-text-secondary);
  background: rgba(255, 255, 255, 0.03);
}

.ws-chevron {
  opacity: 0.7;
  flex-shrink: 0;
}

.ws-name {
  flex: 1 1 auto;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  pointer-events: none;
}

.ws-count {
  font-size: 9px;
  opacity: 0.6;
  font-weight: 500;
  letter-spacing: 0;
  text-transform: none;
  pointer-events: none;
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

.status-dot.ready {
  background: var(--color-success);
  opacity: 0.7;
}

.status-dot.attention {
  background: #e5a03c;
  animation: pulse-running 1.2s ease-in-out infinite;
}

@keyframes pulse-running {
  0%, 100% { opacity: 0.5; transform: scale(0.9); }
  50% { opacity: 1; transform: scale(1.1); }
}
</style>
