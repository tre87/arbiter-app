<script setup lang="ts">
import { computed, ref, nextTick, onMounted, onBeforeUnmount } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { usePaneStore } from '../stores/pane'
import { useProjectStore } from '../stores/project'
import { useConfirm } from '../composables/useConfirm'
import MdiIcon from './MdiIcon.vue'
import { mdiClose, mdiFolder, mdiConsole } from '@mdi/js'

const store = usePaneStore()
const projectStore = useProjectStore()
const { confirm: confirmDialog } = useConfirm()

// Floor the container at sum(tab min-widths) + + button + gaps + padding so
// the parent flex (titlebar) shrinks the stats bar before chopping any tab or
// hiding the + button. Tabs still compress inside this floor as the window
// narrows — each tab's own min-width: 86px keeps "Xxx…" readable; once tabs
// are all at that floor and the container is at this overall floor, further
// pressure flows entirely into the stats bar.
const TAB_MIN_PX = 86
const TAB_GAP_PX = 3
const ADD_BTN_PX = 26
const CONTAINER_GAPS_AND_PAD_PX = 14 // 2 gaps between (scroll, +, spacer) + 8 padding
const tabsContainerMinWidth = computed(() => {
  const count = Math.max(1, store.workspaces.length)
  const tabsRow = count * TAB_MIN_PX + (count - 1) * TAB_GAP_PX
  return `${tabsRow + ADD_BTN_PX + CONTAINER_GAPS_AND_PAD_PX}px`
})

async function confirmAndCloseWorkspace(index: number) {
  const ws = store.workspaces[index]
  if (!ws) return
  const ok = await confirmDialog({
    title: `Close workspace "${ws.name}"?`,
    message: ws.type === 'project'
      ? 'All terminals in this project workspace will be closed.'
      : 'All terminals in this workspace will be closed.',
    confirmText: 'Close',
    danger: true,
  })
  if (!ok) return
  // Closing the only workspace would leave the app in an empty state, so add
  // a fresh terminal workspace first; removeWorkspace's length-guard then
  // accepts the removal because length is back to >1.
  if (store.workspaces.length <= 1) store.addWorkspace()
  store.removeWorkspace(index)
}

// ── New workspace dropdown ──────────────────────────────────────────────────
const showNewMenu = ref(false)
const addBtnEl = ref<HTMLElement | null>(null)
const addMenuPos = ref<{ x: number; y: number } | null>(null)

function toggleNewMenu() {
  if (showNewMenu.value) {
    showNewMenu.value = false
    return
  }
  // Position below the + button
  if (addBtnEl.value) {
    const rect = addBtnEl.value.getBoundingClientRect()
    addMenuPos.value = { x: rect.left, y: rect.bottom + 2 }
  }
  showNewMenu.value = true
}

function closeNewMenu() {
  showNewMenu.value = false
}

function newTerminalWorkspace() {
  closeNewMenu()
  store.addWorkspace()
}

async function openProjectAt(repoRoot: string) {
  const result = await projectStore.createProjectWorkspace(repoRoot)
  if (result.kind === 'ok' || result.kind === 'error') return

  // not-main: the user picked a linked worktree. Warn and offer to open the
  // main repo instead (Arbiter's model requires the workspace to be anchored
  // at the main worktree).
  const branchNote = result.pickedBranch ? ` ("${result.pickedBranch}")` : ''
  const ok = await confirmDialog({
    title: `"${result.repoName}" is a worktree, not the main repo`,
    message:
      `The folder you selected${branchNote} is a linked Git worktree. ` +
      `Arbiter project workspaces must be opened at the main repository, ` +
      `which is at "${result.mainPath}". Open the main repo instead?`,
    confirmText: 'Open main repo',
    cancelText: 'Cancel',
  })
  if (ok) await projectStore.createProjectWorkspace(result.mainPath)
}

async function newProjectWorkspace() {
  closeNewMenu()
  try {
    const { open } = await import('@tauri-apps/plugin-dialog')
    const selected = await open({ directory: true, title: 'Select Project Folder' })
    if (!selected || typeof selected !== 'string') return

    const repoRoot = await invoke<string | null>('git_repo_root', { path: selected })
    if (!repoRoot) return

    await openProjectAt(repoRoot)
  } catch (e) {
    console.error('Failed to create project workspace:', e)
  }
}

// ── Inline rename ───────────────────────────────────────────────────────────
const editingIndex = ref<number | null>(null)
const editValue = ref('')
const editInput = ref<HTMLInputElement | null>(null)

function startRename(index: number) {
  closeContextMenu()
  editingIndex.value = index
  editValue.value = store.workspaces[index].name
  nextTick(() => {
    editInput.value?.focus()
    editInput.value?.select()
  })
}

function finishRename() {
  if (editingIndex.value == null) return
  const name = editValue.value.trim()
  if (name) {
    store.renameWorkspace(editingIndex.value, name)
  }
  editingIndex.value = null
}

function cancelRename() {
  editingIndex.value = null
}

function onRenameKeydown(e: KeyboardEvent) {
  if (e.key === 'Enter') { e.preventDefault(); finishRename() }
  else if (e.key === 'Escape') { e.preventDefault(); cancelRename() }
}

// ── Context menu ────────────────────────────────────────────────────────────
const contextMenu = ref<{ x: number; y: number; index: number } | null>(null)

function onContextMenu(e: MouseEvent, index: number) {
  e.preventDefault()
  contextMenu.value = { x: e.clientX, y: e.clientY, index }
}

function closeContextMenu() {
  contextMenu.value = null
}

function onContextRename() {
  if (contextMenu.value) startRename(contextMenu.value.index)
}

function onContextClose() {
  if (contextMenu.value) {
    const index = contextMenu.value.index
    closeContextMenu()
    confirmAndCloseWorkspace(index)
  }
}

function onDocumentClick() {
  closeContextMenu()
  closeNewMenu()
}

onMounted(() => document.addEventListener('click', onDocumentClick))
onBeforeUnmount(() => document.removeEventListener('click', onDocumentClick))

// ── Tab click ───────────────────────────────────────────────────────────────
function onTabClick(index: number) {
  if (editingIndex.value === index) return
  store.switchWorkspace(index)
}

function onTabMouseDown(e: MouseEvent, index: number) {
  if (e.button === 1) {
    e.preventDefault()
    confirmAndCloseWorkspace(index)
  }
}

// ── Horizontal scroll on vertical wheel ─────────────────────────────────────
const tabsContainer = ref<HTMLElement | null>(null)

function onWheel(e: WheelEvent) {
  if (!tabsContainer.value) return
  // Convert vertical scroll to horizontal
  if (e.deltaY !== 0) {
    e.preventDefault()
    tabsContainer.value.scrollLeft += e.deltaY
  }
}

// ── Drag reorder (pointer-based, works inside -webkit-app-region: drag) ─────
const dragIndex = ref<number | null>(null)
const dragOverIndex = ref<number | null>(null)

let dragStartX = 0
let dragStarted = false
const DRAG_THRESHOLD = 5

function onPointerDown(e: PointerEvent, index: number) {
  if (e.button !== 0) return
  dragStartX = e.clientX
  dragStarted = false
  const pointerId = e.pointerId

  // Use document-level listeners so we track the pointer everywhere
  const onPointerMove = (ev: PointerEvent) => {
    if (ev.pointerId !== pointerId) return
    if (!dragStarted && Math.abs(ev.clientX - dragStartX) >= DRAG_THRESHOLD) {
      dragStarted = true
      dragIndex.value = index
    }
    if (!dragStarted || !tabsContainer.value) return
    // Hit-test by comparing pointer X against each tab's bounding rect
    const tabs = Array.from(tabsContainer.value.querySelectorAll('.tab')) as HTMLElement[]
    let overIdx: number | null = null
    for (let t = 0; t < tabs.length; t++) {
      const rect = tabs[t].getBoundingClientRect()
      if (ev.clientX >= rect.left && ev.clientX < rect.right &&
          ev.clientY >= rect.top && ev.clientY < rect.bottom) {
        overIdx = t
        break
      }
    }
    dragOverIndex.value = overIdx
  }

  const onPointerUp = (ev: PointerEvent) => {
    if (ev.pointerId !== pointerId) return
    document.removeEventListener('pointermove', onPointerMove)
    document.removeEventListener('pointerup', onPointerUp)
    if (dragStarted && dragIndex.value != null && dragOverIndex.value != null && dragIndex.value !== dragOverIndex.value) {
      store.moveWorkspace(dragIndex.value, dragOverIndex.value)
    }
    dragIndex.value = null
    dragOverIndex.value = null
    dragStarted = false
  }

  document.addEventListener('pointermove', onPointerMove)
  document.addEventListener('pointerup', onPointerUp)
}
</script>

<template>
  <div class="workspace-tabs" :style="{ minWidth: tabsContainerMinWidth }">
    <div ref="tabsContainer" class="tabs-scroll" @wheel="onWheel">
      <div
        v-for="(ws, i) in store.workspaces"
        :key="ws.id"
        class="tab"
        :class="{
          active: i === store.activeWorkspaceIndex,
          'drag-over': dragOverIndex === i && dragIndex !== i,
          dragging: dragIndex === i,
        }"
        :title="ws.name"
        @click="onTabClick(i)"
        @mousedown="onTabMouseDown($event, i)"
        @pointerdown="onPointerDown($event, i)"
        @contextmenu="onContextMenu($event, i)"
      >
        <input
          v-if="editingIndex === i"
          ref="editInput"
          v-model="editValue"
          class="tab-rename-input"
          maxlength="40"
          @blur="finishRename"
          @keydown="onRenameKeydown"
          @click.stop
        />
        <template v-else>
          <span
            v-if="store.getWorkspaceStatus(i) !== 'idle'"
            class="tab-status"
            :class="'st-' + store.getWorkspaceStatus(i)"
            :title="store.getWorkspaceStatus(i) === 'attention' ? 'Needs attention'
              : store.getWorkspaceStatus(i) === 'working' ? 'Claude working' : 'Running'"
          />
          <MdiIcon v-if="ws.type === 'project'" :path="mdiFolder" :size="12" class="tab-type-icon" />
          <MdiIcon v-else :path="mdiConsole" :size="12" class="tab-type-icon" />
          <span class="tab-label">{{ ws.name }}</span>
        </template>
        <button
          v-if="editingIndex !== i"
          class="tab-close"
          title="Close workspace"
          @click.stop="confirmAndCloseWorkspace(i)"
        >
          <MdiIcon :path="mdiClose" :size="12" />
        </button>
      </div>
    </div>
    <button ref="addBtnEl" class="tab-add" title="New workspace (Ctrl+Shift+T)" @click.stop="toggleNewMenu">
      +
    </button>
    <div class="tab-drag-spacer" />
  </div>

  <!-- New workspace menu -->
  <Teleport to="body">
    <div
      v-if="showNewMenu && addMenuPos"
      class="new-menu"
      :style="{ left: addMenuPos.x + 'px', top: addMenuPos.y + 'px' }"
      @click.stop
    >
      <button class="new-menu-item" @click="newTerminalWorkspace">
        <MdiIcon :path="mdiConsole" :size="14" />
        Terminal Workspace
      </button>
      <button class="new-menu-item" @click="newProjectWorkspace">
        <MdiIcon :path="mdiFolder" :size="14" />
        Project Workspace
      </button>
    </div>
  </Teleport>

  <!-- Context menu -->
  <Teleport to="body">
    <div
      v-if="contextMenu"
      class="tab-context-menu"
      :style="{ left: contextMenu.x + 'px', top: contextMenu.y + 'px' }"
      @click.stop
    >
      <button class="ctx-item" @click="onContextRename">Rename</button>
      <button
        class="ctx-item danger"
        @click="onContextClose"
      >
        Close
      </button>
    </div>
  </Teleport>
</template>

<style scoped>
.workspace-tabs {
  display: flex;
  align-items: center;
  gap: 3px;
  height: 100%;
  min-width: 0;
  padding: 0 4px;
}

/* Inner scroll region: only the tab list scrolls/clips. The + button and the
   drag spacer sit outside this so + is always visible at a deterministic spot
   regardless of how many tabs there are or how narrow the window gets.
   grow: 0 keeps it at content width when the window is wide (no gap between
   the last tab and the + button); shrink: 1 lets it absorb overflow when
   the window narrows. min-width: 0 lets it compress all the way down — the
   per-tab min-width still acts as the floor for each individual tab. If the
   container can't fit every tab at that floor, the rightmost one will start
   to clip; that's the trade-off for actually getting visible compression. */
.tabs-scroll {
  display: flex;
  align-items: center;
  gap: 3px;
  height: 100%;
  min-width: 0;
  flex: 0 1 auto;
  overflow-x: auto;
  overflow-y: hidden;
  scrollbar-width: none;
}

.tabs-scroll::-webkit-scrollbar {
  display: none;
}

.tab {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 0 28px 0 8px;
  height: 26px;
  margin: auto 0;
  /* 8 (pad-left) + 14 (icon + margin) + 4 (gap) + ~32 ("Xxx…") + 28 (pad-right
     for absolutely-positioned close button) — enough to always show 3 label
     chars followed by an ellipsis. */
  min-width: 86px;
  max-width: 240px;
  flex-shrink: 1;
  cursor: pointer;
  color: var(--color-text-secondary);
  font-size: 12px;
  line-height: 1.2;
  font-weight: 400;
  white-space: nowrap;
  background: transparent;
  border: 1px solid rgba(255, 255, 255, 0.05);
  border-radius: var(--radius-md);
  transition: color 0.12s, background 0.12s, border-color 0.12s;
  position: relative;
  overflow: hidden;
  touch-action: none;
}

.tab:hover {
  color: var(--color-text-primary);
  background: rgba(255, 255, 255, 0.04);
  border-color: rgba(255, 255, 255, 0.1);
}

.tab.active {
  color: var(--color-text-primary);
  background: rgba(255, 255, 255, 0.06);
  border-color: rgba(255, 255, 255, 0.14);
  backdrop-filter: blur(8px);
}

.tab.dragging {
  opacity: 0.4;
}

.tab.drag-over::before {
  content: '';
  position: absolute;
  left: -1px;
  top: 25%;
  bottom: 25%;
  width: 2px;
  background: var(--color-accent);
  border-radius: 1px;
}

.tab-label {
  flex: 1 1 auto;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  pointer-events: none;
}

.tab-rename-input {
  background: var(--color-bg);
  border: 1px solid var(--color-accent);
  border-radius: 3px;
  color: var(--color-text-primary);
  font-size: 12px;
  font-family: inherit;
  padding: 1px 4px;
  width: 100px;
  outline: none;
}

.tab-close {
  position: absolute;
  right: 4px;
  top: 50%;
  transform: translateY(-50%);
  display: flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  background: none;
  border: none;
  border-radius: var(--radius-sm);
  color: var(--color-text-muted);
  font-size: 14px;
  line-height: 1;
  cursor: pointer;
  padding: 0;
  flex-shrink: 0;
  opacity: 0.55;
  transition: opacity 0.12s, background 0.12s, color 0.12s;
}

.tab:hover .tab-close,
.tab.active .tab-close {
  opacity: 1;
}

.tab-close:hover {
  background: rgba(255, 255, 255, 0.1);
  color: var(--color-text-primary);
  opacity: 1;
}

/* Mirrors .tab so the + button reads as part of the same control group. */
.tab-add {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 26px;
  min-width: 26px;
  height: 26px;
  margin: auto 0;
  flex-shrink: 0;
  cursor: pointer;
  color: var(--color-text-secondary);
  background: transparent;
  border: 1px solid rgba(255, 255, 255, 0.05);
  border-radius: var(--radius-md);
  font-size: 16px;
  line-height: 1;
  transition: color 0.12s, background 0.12s, border-color 0.12s;
  padding: 0;
}

.tab-add:hover {
  color: var(--color-text-primary);
  background: rgba(255, 255, 255, 0.04);
  border-color: rgba(255, 255, 255, 0.1);
}

.tab-type-icon {
  flex-shrink: 0;
  display: block;
  opacity: 0.5;
  margin-right: 2px;
}

/* Aggregated workspace status dot (attention > working > running). Shares the
   colour language used by the overview and worktree cards. */
.tab-status {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  flex-shrink: 0;
  margin-right: 1px;
}
.tab-status.st-attention { background: #e5a03c; animation: tab-status-pulse 1.2s ease-in-out infinite; }
.tab-status.st-working   { background: var(--azure); animation: tab-status-pulse 1.5s ease-in-out infinite; }
.tab-status.st-running   { background: var(--color-success); animation: tab-status-pulse 1.5s ease-in-out infinite; }
@keyframes tab-status-pulse {
  0%, 100% { opacity: 1; }
  50%      { opacity: 0.35; }
}

.new-menu {
  position: fixed;
  z-index: 9999;
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-card-border);
  border-radius: 6px;
  padding: 4px 0;
  min-width: 180px;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.4);
}

.new-menu-item {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  background: none;
  border: none;
  color: var(--color-text-secondary);
  font-size: 12px;
  font-family: inherit;
  padding: 6px 12px;
  text-align: left;
  cursor: pointer;
  transition: background 0.1s, color 0.1s;
}

.new-menu-item:hover {
  background: var(--color-accent, var(--azure));
  color: #fff;
}

.tab-drag-spacer {
  flex: 1 1 auto;
  min-width: 0;
  height: 100%;
}

/* Context menu */
.tab-context-menu {
  position: fixed;
  z-index: 9999;
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-card-border);
  border-radius: 6px;
  padding: 4px 0;
  min-width: 120px;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.4);
}

.ctx-item {
  display: block;
  width: 100%;
  background: none;
  border: none;
  color: var(--color-text-secondary);
  font-size: 12px;
  font-family: inherit;
  padding: 6px 12px;
  text-align: left;
  cursor: pointer;
  transition: background 0.1s, color 0.1s;
}

.ctx-item:hover {
  background: var(--color-accent);
  color: #fff;
}

.ctx-item.danger:hover {
  background: var(--color-danger, #e81123);
}
</style>
