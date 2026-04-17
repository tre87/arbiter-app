<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import { useProjectStore, type DirEntry } from '../stores/project'
import MdiIcon from './MdiIcon.vue'
import { mdiChevronRight, mdiChevronDown } from '@mdi/js'
import { getFileIcon } from '../utils/fileIcons'

const props = defineProps<{
  entry: DirEntry
  worktreeId: string
  worktreePath: string
  expandedPaths: Set<string>
  selection: Set<string>
  renamingPath: string | null
  depth: number
}>()

const emit = defineEmits<{
  select: [event: MouseEvent, entry: DirEntry]
  open: [entry: DirEntry]
  contextmenu: [event: MouseEvent, entry: DirEntry]
  'rename-commit': [path: string, newName: string]
  'rename-cancel': []
}>()

const isSelected = computed(() => props.selection.has(props.entry.path))

const projectStore = useProjectStore()

const isRenaming = computed(() => props.renamingPath === props.entry.path)
const renameValue = ref('')
const renameInputEl = ref<HTMLInputElement | null>(null)

watch(isRenaming, (active) => {
  if (active) {
    renameValue.value = props.entry.name
    nextTick(() => {
      const el = renameInputEl.value
      if (!el) return
      el.focus()
      const dot = props.entry.is_dir ? -1 : props.entry.name.lastIndexOf('.')
      if (dot > 0) el.setSelectionRange(0, dot)
      else el.select()
    })
  }
})

function commitRename() {
  const trimmed = renameValue.value.trim()
  if (!trimmed || trimmed === props.entry.name) {
    emit('rename-cancel')
    return
  }
  emit('rename-commit', props.entry.path, trimmed)
}

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

const isExpanded = computed(() => props.expandedPaths.has(props.entry.path))
const children = computed(() =>
  props.entry.is_dir && isExpanded.value
    ? (projectStore.getCachedDirectory(props.worktreeId, props.entry.path) ?? [])
    : [],
)
const indent = `${props.depth * 16}px`
</script>

<template>
  <div
    class="tree-item"
    :class="{ dir: entry.is_dir, selected: isSelected }"
    :style="{ paddingLeft: `calc(8px + ${indent})` }"
    @click="!isRenaming && emit('select', $event, entry)"
    @dblclick="!isRenaming && !entry.is_dir && emit('open', entry)"
    @contextmenu.prevent.stop="emit('contextmenu', $event, entry)"
  >
    <span v-if="entry.is_dir" class="tree-chevron">
      <MdiIcon v-if="isExpanded" :path="mdiChevronDown" :size="16" />
      <MdiIcon v-else :path="mdiChevronRight" :size="16" />
    </span>
    <span v-else class="tree-file-icon">
      <MdiIcon :path="getFileIcon(entry.name).icon" :size="16" :style="{ color: getFileIcon(entry.name).color }" />
    </span>
    <input
      v-if="isRenaming"
      ref="renameInputEl"
      v-model="renameValue"
      class="tree-rename-input"
      @click.stop
      @contextmenu.stop
      @keydown.enter.prevent="commitRename"
      @keydown.escape.prevent="emit('rename-cancel')"
      @blur="commitRename"
    />
    <span v-else class="tree-name" :style="{ color: getStatusColor(entry) }">{{ entry.name }}</span>
  </div>
  <template v-if="entry.is_dir && isExpanded">
    <FileExplorerNode
      v-for="child in children"
      :key="child.path"
      :entry="child"
      :worktree-id="worktreeId"
      :worktree-path="worktreePath"
      :expanded-paths="expandedPaths"
      :selection="selection"
      :renaming-path="renamingPath"
      :depth="depth + 1"
      @select="(e, c) => emit('select', e, c)"
      @open="(c) => emit('open', c)"
      @contextmenu="(e, c) => emit('contextmenu', e, c)"
      @rename-commit="(p, n) => emit('rename-commit', p, n)"
      @rename-cancel="emit('rename-cancel')"
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
.tree-item.selected {
  background: var(--color-accent-subtle, rgba(51, 153, 255, 0.18));
}
.tree-item.selected:hover {
  background: var(--color-accent-subtle, rgba(51, 153, 255, 0.24));
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

.tree-rename-input {
  flex: 1;
  min-width: 0;
  font: inherit;
  color: var(--color-text-primary);
  background: var(--color-bg);
  border: 1px solid var(--color-accent);
  border-radius: 3px;
  padding: 0 4px;
  outline: none;
}
</style>
