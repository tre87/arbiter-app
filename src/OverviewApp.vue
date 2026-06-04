<script setup lang="ts">
import { ref, onMounted, onBeforeUnmount, computed } from 'vue'
import { listen, emit, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import ClaudeWorkingIcon from './components/ClaudeWorkingIcon.vue'
import ClaudeIcon from './components/ClaudeIcon.vue'
import MdiIcon from './components/MdiIcon.vue'
import {
  mdiConsole, mdiFolder, mdiChevronRight, mdiChevronDown,
  mdiCheckCircleOutline, mdiCircleEditOutline, mdiPlusCircleOutline, mdiArrowUp, mdiArrowDown,
} from '@mdi/js'
import type { GitInfo } from './types/pane'

interface TerminalInfo {
  paneId: string
  workspaceId: string
  workspaceIndex: number
  workspaceName: string
  workspaceType: 'terminal' | 'project'
  name: string
  status: 'idle' | 'running' | 'ready' | 'working' | 'attention'
  claudeActive: boolean
  gitInfo?: GitInfo | null
}

interface OverviewUpdate {
  terminals: TerminalInfo[]
  claudeOnly: boolean
}

interface WorkspaceGroup {
  workspaceId: string
  workspaceName: string
  workspaceIndex: number
  workspaceType: 'terminal' | 'project'
  terminals: TerminalInfo[]
}

const terminals = ref<TerminalInfo[]>([])
const claudeOnly = ref(true)
const error = ref('')
const collapsed = ref<Set<string>>(new Set())
let unlistenUpdate: UnlistenFn | null = null
let unlistenResized: UnlistenFn | null = null
let unlistenMoved: UnlistenFn | null = null
let geomTimer: ReturnType<typeof setTimeout> | null = null

// This window's move/resize lives in its own webview, invisible to the main
// window's autosave geometry listeners. Notify the main window (debounced) so it
// persists our size/position — otherwise it's only captured at quit and lost on
// Cmd+Q / a missed flush.
function notifyGeometryChanged() {
  if (geomTimer) clearTimeout(geomTimer)
  geomTimer = setTimeout(() => emit('overview-geometry-changed'), 200)
}

const grouped = computed<WorkspaceGroup[]>(() => {
  const groups: WorkspaceGroup[] = []
  let currentGroup: WorkspaceGroup | null = null
  for (const t of terminals.value) {
    if (claudeOnly.value && !t.claudeActive) continue
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

// Right-click anywhere → a small menu to toggle the Claude-only filter. The
// setting lives in the main window's store, so we optimistically flip the local
// copy and tell the main window to update (and persist) it.
const menu = ref<{ x: number; y: number } | null>(null)

function handleContextMenu(e: MouseEvent) {
  e.preventDefault()
  menu.value = { x: e.clientX, y: e.clientY }
}

function closeMenu() { menu.value = null }

function toggleClaudeOnly() {
  const next = !claudeOnly.value
  claudeOnly.value = next
  emit('overview-set-claude-only', next)
  closeMenu()
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
    unlistenUpdate = await listen<OverviewUpdate>('overview-update', (event) => {
      terminals.value = event.payload.terminals
      claudeOnly.value = event.payload.claudeOnly
    })

    // Request initial state from main window
    await emit('overview-request-update')

    const win = getCurrentWindow()
    unlistenResized = await win.onResized(notifyGeometryChanged)
    unlistenMoved = await win.onMoved(notifyGeometryChanged)
  } catch (e: any) {
    error.value = String(e)
  }

  window.addEventListener('keydown', onKeydown)
  window.addEventListener('contextmenu', handleContextMenu)
  window.addEventListener('click', closeMenu)
})

function onKeydown(e: KeyboardEvent) {
  if (e.key !== 'Escape') return
  if (menu.value) closeMenu()
  else hideWindow()
}

onBeforeUnmount(() => {
  unlistenUpdate?.()
  unlistenResized?.()
  unlistenMoved?.()
  if (geomTimer) clearTimeout(geomTimer)
  window.removeEventListener('keydown', onKeydown)
  window.removeEventListener('contextmenu', handleContextMenu)
  window.removeEventListener('click', closeMenu)
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
            <span class="overview-left">
              <ClaudeIcon v-if="t.claudeActive" :size="12" class="overview-claude-icon" />
              <span class="overview-name">{{ t.name }}</span>
            </span>
            <span v-if="t.gitInfo?.is_repo" class="overview-git">
              <span v-if="t.gitInfo.staged" class="git-staged" title="Staged">
                <MdiIcon :path="mdiCheckCircleOutline" :size="12" /><span class="git-num">{{ t.gitInfo.staged }}</span>
              </span>
              <span v-if="t.gitInfo.unstaged" class="git-unstaged" title="Modified">
                <MdiIcon :path="mdiCircleEditOutline" :size="12" /><span class="git-num">{{ t.gitInfo.unstaged }}</span>
              </span>
              <span v-if="t.gitInfo.untracked" class="git-untracked" title="Untracked">
                <MdiIcon :path="mdiPlusCircleOutline" :size="12" /><span class="git-num">{{ t.gitInfo.untracked }}</span>
              </span>
              <span v-if="t.gitInfo.ahead || t.gitInfo.behind" class="git-commits" title="Ahead / behind">
                <span v-if="t.gitInfo.ahead" class="gc"><MdiIcon :path="mdiArrowUp" :size="11" /><span class="git-num">{{ t.gitInfo.ahead }}</span></span>
                <span v-if="t.gitInfo.behind" class="gc"><MdiIcon :path="mdiArrowDown" :size="11" /><span class="git-num">{{ t.gitInfo.behind }}</span></span>
              </span>
            </span>
            <span class="overview-status">
              <ClaudeWorkingIcon v-if="t.status === 'working'" :size="18" />
              <span v-else class="status-dot" :class="t.status" />
            </span>
          </div>
        </div>
      </div>
    </div>

    <!-- Right-click context menu -->
    <div
      v-if="menu"
      class="overview-menu"
      :style="{ left: menu.x + 'px', top: menu.y + 'px' }"
      @click.stop
    >
      <button class="overview-menu-item" @click="toggleClaudeOnly">
        {{ claudeOnly ? 'Show all terminals' : 'Show only Claude terminals' }}
      </button>
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
  font-size: 18px;
  cursor: pointer;
  /* Square box, centered in the shared right gutter so the × lines up with the
     workspace counts and status dots below it. */
  width: 20px;
  height: 20px;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 0;
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
  padding: 4px 8px 2px 12px;
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
  /* Centered in the shared right slot so counts line up with the × and dots. */
  width: 20px;
  text-align: center;
  flex-shrink: 0;
}

.overview-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 5px 8px 5px 12px;
  cursor: pointer;
  transition: background 0.1s;
}

.overview-row:hover {
  background: var(--color-card-border);
}

.overview-left {
  display: flex;
  align-items: center;
  gap: 5px;
  min-width: 0;
  flex: 1 1 auto;
}

/* Compact git stats, mirroring the terminal footer. Right-aligned next to the
   status dot; never squished (the name truncates instead). */
.overview-git {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
  margin-left: 6px;
  font-size: 11px;
  font-weight: 600;
}
.overview-git > span { display: inline-flex; align-items: center; gap: 2px; }
.overview-git svg { display: block; }
.overview-git .git-num { display: block; transform: translateY(1px); }
.overview-git .git-staged { color: #6a9955; }
.overview-git .git-unstaged { color: #e5a03c; }
.overview-git .git-untracked { color: #569cd6; }
.overview-git .git-commits { color: var(--color-text-muted); gap: 4px; }
.overview-git .gc { display: inline-flex; align-items: center; gap: 1px; }

.overview-claude-icon {
  flex-shrink: 0;
  display: block;
}

.overview-name {
  font-size: 12px;
  color: var(--color-text-primary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

/* Fixed-width, centered slot so the working glyph and the 6px dot share the
   same centre (otherwise the wider glyph sits left of the dots). */
.overview-status {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  margin-left: 8px;
  flex-shrink: 0;
}

.status-dot {
  width: 10px;
  height: 10px;
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

/* Claude session alive but idle — neutral grey (green is reserved for an
   actually-running job; the animations cover working / attention). */
.status-dot.ready {
  background: var(--color-text-muted);
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

.overview-menu {
  position: fixed;
  z-index: 100;
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-card-border);
  border-radius: 6px;
  padding: 4px;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.4);
  min-width: 160px;
}

.overview-menu-item {
  display: block;
  width: 100%;
  background: none;
  border: none;
  color: var(--color-text-secondary);
  font-size: 12px;
  font-family: inherit;
  padding: 6px 10px;
  text-align: left;
  cursor: pointer;
  border-radius: 4px;
  transition: background 0.1s, color 0.1s;
}

.overview-menu-item:hover {
  background: var(--color-accent, var(--azure));
  color: #fff;
}
</style>
