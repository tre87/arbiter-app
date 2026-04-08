<script setup lang="ts">
import { ref, watch, onMounted, onBeforeUnmount } from 'vue'
import { listen } from '@tauri-apps/api/event'
import { useProjectStore, type DirEntry } from '../stores/project'
import FileExplorerNode from './FileExplorerNode.vue'
import MdiIcon from './MdiIcon.vue'
import { mdiChevronRight, mdiChevronDown } from '@mdi/js'
import { getFileIcon } from '../utils/fileIcons'
import type { Worktree } from '../types/pane'

const props = defineProps<{
  worktree: Worktree
  workspaceRepoRoot: string
}>()

const projectStore = useProjectStore()

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

// ── Git status colors ───────────────────────────────────────────────────────

function getStatusColor(entry: DirEntry): string | undefined {
  const relativePath = entry.path.replace(props.worktree.path, '').replace(/^[/\\]/, '').replace(/\\/g, '/')
  if (entry.is_dir) {
    return statusToColor(projectStore.getFolderStatus(props.worktree.id, relativePath))
  }
  return statusToColor(projectStore.getFileStatus(props.worktree.id, relativePath))
}

function statusToColor(status: string | undefined): string | undefined {
  switch (status) {
    case 'modified': return '#e2c08d'
    case 'added': return '#73c991'
    case 'deleted': return '#c74e39'
    case 'untracked': return '#73c991'
    case 'conflicted': return '#e5c07b'
    case 'renamed': return '#73c991'
    default: return undefined
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
    <div class="explorer-tree">
      <template v-for="entry in rootEntries" :key="entry.path">
        <div
          class="tree-item"
          :class="{ dir: entry.is_dir }"
          @click="entry.is_dir && toggleExpand(entry.path)"
        >
          <span v-if="entry.is_dir" class="tree-chevron">
            <MdiIcon v-if="expandedPaths.has(entry.path)" :path="mdiChevronDown" :size="16" />
            <MdiIcon v-else :path="mdiChevronRight" :size="16" />
          </span>
          <span v-else class="tree-file-icon">
            <MdiIcon :path="getFileIcon(entry.name).icon" :size="16" :style="{ color: getFileIcon(entry.name).color }" />
          </span>
          <span class="tree-name" :style="{ color: getStatusColor(entry) }">{{ entry.name }}</span>
        </div>
        <template v-if="entry.is_dir && expandedPaths.has(entry.path)">
          <FileExplorerNode
            v-for="child in projectStore.getCachedDirectory(worktree.id, entry.path) ?? []"
            :key="child.path"
            :entry="child"
            :worktree-id="worktree.id"
            :worktree-path="worktree.path"
            :expanded-paths="expandedPaths"
            :depth="1"
            @toggle="toggleExpand"
          />
        </template>
      </template>
    </div>
  </div>
</template>

<style scoped>
.file-explorer {
  width: 220px;
  min-width: 160px;
  display: flex;
  flex-direction: column;
  background: var(--color-bg);
  border-right: 1px solid var(--color-card-border);
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

.tree-item {
  display: flex;
  align-items: center;
  padding: 2px 8px;
  gap: 4px;
  cursor: default;
  color: var(--color-text-primary);
  white-space: nowrap;
  height: 24px;
}
.tree-item.dir {
  cursor: pointer;
}
.tree-item:hover {
  background: var(--color-bg-subtle);
}

.tree-chevron {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  flex-shrink: 0;
  color: var(--color-text-muted);
}

.tree-indent {
  width: 18px;
  flex-shrink: 0;
}

.tree-file-icon {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  flex-shrink: 0;
}

.tree-name {
  overflow: hidden;
  text-overflow: ellipsis;
}
</style>
