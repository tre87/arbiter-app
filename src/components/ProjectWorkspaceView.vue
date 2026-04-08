<script setup lang="ts">
import { computed } from 'vue'
import FileExplorer from './FileExplorer.vue'
import SplitView from './SplitView.vue'
import WorktreePanel from './WorktreePanel.vue'
import type { ProjectWorkspace } from '../types/pane'

const props = defineProps<{
  workspace: ProjectWorkspace
}>()

const activeWorktree = computed(() =>
  props.workspace.worktrees.find(w => w.id === props.workspace.activeWorktreeId)
)

const centerRoot = computed(() => activeWorktree.value?.root ?? null)
</script>

<template>
  <div class="project-workspace">
    <FileExplorer
      v-if="activeWorktree"
      :key="activeWorktree.id"
      :worktree="activeWorktree"
      :workspace-repo-root="workspace.repoRoot"
    />
    <div class="center-content">
      <SplitView
        v-if="centerRoot"
        :node="centerRoot"
        :key="workspace.activeWorktreeId"
      />
    </div>
    <WorktreePanel :workspace="workspace" />
  </div>
</template>

<style scoped>
.project-workspace {
  display: flex;
  flex: 1;
  height: 100%;
  min-height: 0;
  overflow: hidden;
}

.center-content {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
}
</style>
