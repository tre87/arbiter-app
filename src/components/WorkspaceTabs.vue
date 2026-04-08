<script setup lang="ts">
import { ref, nextTick, onMounted, onBeforeUnmount } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { usePaneStore } from '../stores/pane'
import { useProjectStore } from '../stores/project'
import { useConfirm } from '../composables/useConfirm'
import MdiIcon from './MdiIcon.vue'
import { mdiClose, mdiFolder, mdiConsole } from '@mdi/js'

const store = usePaneStore()
const projectStore = useProjectStore()
const { confirm: confirmDialog } = useConfirm()

async function confirmAndCloseWorkspace(index: number) {
  const ws = store.workspaces[index]
  if (!ws || store.workspaces.length <= 1) return
  const ok = await confirmDialog({
    title: `Close workspace "${ws.name}"?`,
    message: ws.type === 'project'
      ? 'All terminals in this project workspace will be closed.'
      : 'All terminals in this workspace will be closed.',
    confirmText: 'Close',
    danger: true,
  })
  if (ok) store.removeWorkspace(index)
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

async function newProjectWorkspace() {
  closeNewMenu()
  try {
    // Use Tauri dialog plugin to pick folder
    const { open } = await import('@tauri-apps/plugin-dialog')
    const selected = await open({ directory: true, title: 'Select Project Folder' })
    if (!selected || typeof selected !== 'string') return

    // Verify it's a git repo
    const repoRoot = await invoke<string | null>('git_repo_root', { path: selected })
    if (!repoRoot) {
      // Not a git repo — could show error, for now just use the selected path
      // and let the worktree commands fail gracefully
      return
    }

    await projectStore.createProjectWorkspace(repoRoot)
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
  <div ref="tabsContainer" class="workspace-tabs" @wheel="onWheel">
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
      <span v-else class="tab-label">
        <MdiIcon v-if="ws.type === 'project'" :path="mdiFolder" :size="12" class="tab-type-icon" />
        <MdiIcon v-else :path="mdiConsole" :size="12" class="tab-type-icon" />
        {{ ws.name.length > 40 ? ws.name.slice(0, 40) + '…' : ws.name }}
      </span>
      <button
        v-if="store.workspaces.length > 1 && editingIndex !== i"
        class="tab-close"
        title="Close workspace"
        @click.stop="confirmAndCloseWorkspace(i)"
      >
        <MdiIcon :path="mdiClose" :size="12" />
      </button>
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
        v-if="store.workspaces.length > 1"
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
  align-items: stretch;
  gap: 1px;
  height: 100%;
  min-width: 0;
  overflow-x: auto;
  overflow-y: hidden;
  scrollbar-width: none;
  -webkit-app-region: no-drag;
}

.workspace-tabs::-webkit-scrollbar {
  display: none;
}

.tab {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 0 8px;
  height: 24px;
  margin: auto 0;
  min-width: 60px;
  max-width: 180px;
  flex-shrink: 1;
  cursor: pointer;
  color: var(--color-text-muted);
  font-size: 12px;
  font-weight: 400;
  white-space: nowrap;
  border-radius: 4px;
  transition: color 0.15s, background 0.15s;
  position: relative;
  overflow: hidden;
  -webkit-app-region: no-drag;
  touch-action: none;
}

.tab:hover {
  color: var(--color-text-secondary);
  background: var(--color-bg-elevated);
}

.tab.active {
  color: var(--color-text-primary);
  background: var(--color-bg-elevated);
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
  display: flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  background: none;
  border: none;
  border-radius: 4px;
  color: var(--color-text-muted);
  font-size: 14px;
  line-height: 1;
  cursor: pointer;
  padding: 0;
  flex-shrink: 0;
  display: none;
}

.tab:hover .tab-close {
  display: flex;
}

.tab-close:hover {
  background: rgba(255, 255, 255, 0.1);
  color: var(--color-text-primary);
}

.tab-add {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  min-width: 28px;
  flex-shrink: 0;
  height: 100%;
  background: none;
  border: none;
  -webkit-app-region: no-drag;
  color: var(--color-text-muted);
  font-size: 18px;
  cursor: pointer;
  transition: color 0.15s, background 0.15s;
  padding: 0;
}

.tab-add:hover {
  color: var(--color-text-primary);
  background: var(--color-bg-elevated);
}

.tab-type-icon {
  opacity: 0.5;
  margin-right: 2px;
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
  -webkit-app-region: drag;
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
