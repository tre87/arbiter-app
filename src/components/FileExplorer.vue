<script setup lang="ts">
import { ref, computed, watch, onMounted, onBeforeUnmount } from 'vue'
import { listen } from '@tauri-apps/api/event'
import { invoke } from '@tauri-apps/api/core'
import { useProjectStore, type DirEntry } from '../stores/project'
import { useConfirm } from '../composables/useConfirm'
import FileExplorerNode from './FileExplorerNode.vue'
import FileExplorerContextMenu from './FileExplorerContextMenu.vue'
import type { Worktree } from '../types/pane'

const props = defineProps<{
  worktree: Worktree
  workspaceRepoRoot: string
}>()

const projectStore = useProjectStore()
const { confirm } = useConfirm()

// ── Expanded state ──────────────────────────────────────────────────────────

const expandedPaths = ref(new Set<string>(props.worktree.explorerExpandedPaths))

function toggleExpand(dirPath: string) {
  if (expandedPaths.value.has(dirPath)) {
    expandedPaths.value.delete(dirPath)
  } else {
    expandedPaths.value.add(dirPath)
    projectStore.loadDirectory(props.worktree.id, dirPath)
  }
  props.worktree.explorerExpandedPaths = [...expandedPaths.value]
}

// ── Root entries ────────────────────────────────────────────────────────────

const rootEntries = ref<DirEntry[]>([])

async function loadRoot() {
  rootEntries.value = await projectStore.loadDirectory(props.worktree.id, props.worktree.path)
  await projectStore.refreshGitStatus(props.worktree.id, props.worktree.path)
}

// ── File watcher ────────────────────────────────────────────────────────────

let watcherId: string | null = null
let unlistenFsChanged: (() => void) | null = null
let refreshDebounce: ReturnType<typeof setTimeout> | null = null

async function setupWatcher() {
  watcherId = await projectStore.watchDirectory(props.worktree.id, props.worktree.path)
  if (watcherId) {
    unlistenFsChanged = await listen(`fs-changed-${watcherId}`, () => {
      if (refreshDebounce) clearTimeout(refreshDebounce)
      refreshDebounce = setTimeout(async () => {
        await loadRoot()
        for (const path of expandedPaths.value) {
          projectStore.loadDirectory(props.worktree.id, path)
        }
      }, 500)
    }) as unknown as (() => void)
  }
}

// ── Selection & flat visible list ───────────────────────────────────────────

const selection = ref(new Set<string>())
// Anchor for shift-range; updated on any non-shift click.
const selectionAnchor = ref<string | null>(null)

// Flat, depth-first walk of currently rendered entries, kept in sync with the
// user's expansion state. `paths` drives shift-range math; `byPath` lets the
// context menu inspect entry metadata (is_dir, name) without another lookup.
const visibleTree = computed(() => {
  const paths: string[] = []
  const byPath = new Map<string, DirEntry>()
  const walk = (entries: DirEntry[]) => {
    for (const e of entries) {
      paths.push(e.path)
      byPath.set(e.path, e)
      if (e.is_dir && expandedPaths.value.has(e.path)) {
        const children = projectStore.getCachedDirectory(props.worktree.id, e.path) ?? []
        walk(children)
      }
    }
  }
  walk(rootEntries.value)
  return { paths, byPath }
})

function onSelect(event: MouseEvent, entry: DirEntry) {
  if (event.shiftKey && selectionAnchor.value) {
    const list = visibleTree.value.paths
    const a = list.indexOf(selectionAnchor.value)
    const b = list.indexOf(entry.path)
    if (a >= 0 && b >= 0) {
      const [start, end] = a < b ? [a, b] : [b, a]
      selection.value = new Set(list.slice(start, end + 1))
    }
    return
  }
  if (event.ctrlKey || event.metaKey) {
    const next = new Set(selection.value)
    if (next.has(entry.path)) next.delete(entry.path)
    else next.add(entry.path)
    selection.value = next
    selectionAnchor.value = entry.path
    return
  }
  // Plain click: single-select and, for directories, toggle expansion.
  selection.value = new Set([entry.path])
  selectionAnchor.value = entry.path
  if (entry.is_dir) toggleExpand(entry.path)
}

async function onOpen(entry: DirEntry) {
  try {
    await invoke('open_path', { path: entry.path })
  } catch (e) {
    console.error('Open failed:', e)
  }
}

// Reset selection when switching worktrees — paths won't exist in the new tree.
watch(() => props.worktree.id, () => {
  selection.value = new Set()
  selectionAnchor.value = null
})

// ── Context menu & rename ───────────────────────────────────────────────────

const ctxMenu = ref<{ x: number; y: number } | null>(null)
const renamingPath = ref<string | null>(null)

const revealLabel = (() => {
  if (navigator.platform.startsWith('Mac')) return 'Reveal in Finder'
  if (navigator.platform.startsWith('Win')) return 'Reveal in File Explorer'
  return 'Open containing folder'
})()

const selectionAllFiles = computed(() => {
  if (selection.value.size === 0) return false
  const byPath = visibleTree.value.byPath
  for (const path of selection.value) {
    if (byPath.get(path)?.is_dir) return false
  }
  return true
})

function onContextMenu(e: MouseEvent, entry: DirEntry) {
  // If the right-clicked item isn't in the current selection, replace the
  // selection with it — so the menu always operates on a meaningful target.
  if (!selection.value.has(entry.path)) {
    selection.value = new Set([entry.path])
    selectionAnchor.value = entry.path
  }
  ctxMenu.value = { x: e.clientX, y: e.clientY }
}

function closeContextMenu() {
  ctxMenu.value = null
}

async function ctxOpen() {
  const paths = [...selection.value]
  closeContextMenu()
  await Promise.all(paths.map(async (path) => {
    try { await invoke('open_path', { path }) }
    catch (e) { console.error('Open failed:', e) }
  }))
}

async function ctxReveal() {
  const path = [...selection.value][0]
  closeContextMenu()
  if (!path) return
  try {
    await invoke('reveal_path', { path })
  } catch (e) {
    console.error('Reveal failed:', e)
  }
}

function ctxStartRename() {
  const path = [...selection.value][0]
  if (!path) return
  renamingPath.value = path
  closeContextMenu()
}

async function ctxDelete() {
  const paths = [...selection.value]
  closeContextMenu()
  if (paths.length === 0) return

  const byPath = visibleTree.value.byPath
  const singleEntry = paths.length === 1 ? byPath.get(paths[0]) : null

  const ok = await confirm({
    title: singleEntry
      ? `Move "${singleEntry.name}" to trash?`
      : `Move ${paths.length} items to trash?`,
    message: singleEntry
      ? (singleEntry.is_dir
          ? 'The folder and all its contents will be moved to the OS trash.'
          : 'The file will be moved to the OS trash.')
      : 'All selected items will be moved to the OS trash.',
    confirmText: 'Delete',
    danger: true,
  })
  if (!ok) return

  const results = await Promise.allSettled(
    paths.map((path) => invoke('trash_path', { path })),
  )
  for (const r of results) {
    if (r.status === 'rejected') console.error('Delete failed:', r.reason)
  }
  selection.value = new Set()
}

async function onRenameCommit(path: string, newName: string) {
  renamingPath.value = null
  try {
    await invoke<string>('rename_path', { oldPath: path, newName })
    // Watcher will refresh entries; expansion state is path-keyed, so a
    // renamed folder loses its expansion state (same behaviour as VS Code).
    selection.value = new Set()
  } catch (e) {
    console.error('Rename failed:', e)
  }
}

function onRenameCancel() {
  renamingPath.value = null
}

// Clicking on the empty area below the tree clears selection, VS Code-style.
function onTreeBackgroundClick(e: MouseEvent) {
  if (e.target === e.currentTarget) {
    selection.value = new Set()
    selectionAnchor.value = null
  }
}

// ── Lifecycle ───────────────────────────────────────────────────────────────

onMounted(async () => {
  await loadRoot()
  await setupWatcher()
  for (const path of expandedPaths.value) {
    projectStore.loadDirectory(props.worktree.id, path)
  }
})

onBeforeUnmount(() => {
  if (refreshDebounce) clearTimeout(refreshDebounce)
  unlistenFsChanged?.()
  if (watcherId) {
    projectStore.unwatchAll(props.worktree.id)
  }
})

watch(() => props.worktree.id, async () => {
  expandedPaths.value = new Set(props.worktree.explorerExpandedPaths)
  await loadRoot()
})
</script>

<template>
  <div class="file-explorer">
    <div class="explorer-header">
      <span class="explorer-title">{{ worktree.branchName }}</span>
    </div>
    <div class="explorer-tree" @click="onTreeBackgroundClick">
      <FileExplorerNode
        v-for="entry in rootEntries"
        :key="entry.path"
        :entry="entry"
        :worktree-id="worktree.id"
        :worktree-path="worktree.path"
        :expanded-paths="expandedPaths"
        :selection="selection"
        :renaming-path="renamingPath"
        :depth="0"
        @select="onSelect"
        @open="onOpen"
        @contextmenu="onContextMenu"
        @rename-commit="onRenameCommit"
        @rename-cancel="onRenameCancel"
      />
    </div>

    <FileExplorerContextMenu
      v-if="ctxMenu"
      :click-x="ctxMenu.x"
      :click-y="ctxMenu.y"
      :selection-count="selection.size"
      :all-files="selectionAllFiles"
      :reveal-label="revealLabel"
      @close="closeContextMenu"
      @open="ctxOpen"
      @reveal="ctxReveal"
      @rename="ctxStartRename"
      @delete="ctxDelete"
    />
  </div>
</template>

<style scoped>
.file-explorer {
  width: 220px;
  min-width: 160px;
  display: flex;
  flex-direction: column;
  background: var(--color-bg-subtle);
  overflow: hidden;
  font-family: system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif;
}

.explorer-header {
  padding: 8px 10px;
  border-bottom: 1px solid var(--color-card-border);
}

.explorer-title {
  font-size: 12px;
  font-weight: 600;
  color: var(--color-text-secondary);
  text-transform: uppercase;
  letter-spacing: 0.5px;
}

.explorer-tree {
  flex: 1;
  overflow-y: auto;
  padding: 4px 0;
  font-size: 13px;
}
</style>
