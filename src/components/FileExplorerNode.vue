<script setup lang="ts">
import { useProjectStore, type DirEntry } from '../stores/project'
import MdiIcon from './MdiIcon.vue'
import { mdiChevronRight, mdiChevronDown } from '@mdi/js'
import { getFileIcon } from '../utils/fileIcons'

const props = defineProps<{
  entry: DirEntry
  worktreeId: string
  worktreePath: string
  expandedPaths: Set<string>
  depth: number
}>()

const emit = defineEmits<{
  toggle: [path: string]
}>()

const projectStore = useProjectStore()

function getStatusColor(entry: DirEntry): string | undefined {
  const relativePath = entry.path.replace(props.worktreePath, '').replace(/^[/\\]/, '').replace(/\\/g, '/')
  const status = entry.is_dir
    ? projectStore.getFolderStatus(props.worktreeId, relativePath)
    : projectStore.getFileStatus(props.worktreeId, relativePath)

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

const isExpanded = props.expandedPaths.has(props.entry.path)
const children = props.entry.is_dir && isExpanded
  ? (projectStore.getCachedDirectory(props.worktreeId, props.entry.path) ?? [])
  : []
const indent = `${props.depth * 16}px`
</script>

<template>
  <div
    class="tree-item"
    :class="{ dir: entry.is_dir }"
    :style="{ paddingLeft: `calc(8px + ${indent})` }"
    @click="entry.is_dir && emit('toggle', entry.path)"
  >
    <span v-if="entry.is_dir" class="tree-chevron">
      <MdiIcon v-if="isExpanded" :path="mdiChevronDown" :size="16" />
      <MdiIcon v-else :path="mdiChevronRight" :size="16" />
    </span>
    <span v-else class="tree-file-icon">
      <MdiIcon :path="getFileIcon(entry.name).icon" :size="16" :style="{ color: getFileIcon(entry.name).color }" />
    </span>
    <span class="tree-name" :style="{ color: getStatusColor(entry) }">{{ entry.name }}</span>
  </div>
  <template v-if="entry.is_dir && isExpanded">
    <FileExplorerNode
      v-for="child in children"
      :key="child.path"
      :entry="child"
      :worktree-id="worktreeId"
      :worktree-path="worktreePath"
      :expanded-paths="expandedPaths"
      :depth="depth + 1"
      @toggle="(path: string) => emit('toggle', path)"
    />
  </template>
</template>

<style scoped>
.tree-item {
  display: flex;
  align-items: center;
  padding: 2px 8px;
  gap: 4px;
  cursor: default;
  color: var(--color-text-primary);
  white-space: nowrap;
  height: 24px;
  font-family: system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif;
  font-size: 13px;
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
