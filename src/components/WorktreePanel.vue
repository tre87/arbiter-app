<script setup lang="ts">
import { ref, computed } from 'vue'
import { useProjectStore } from '../stores/project'
import { usePaneStore } from '../stores/pane'
import WorktreeCard from './WorktreeCard.vue'
import GitMenu from './GitMenu.vue'
import MdiIcon from './MdiIcon.vue'
import WorktreeNewDialog from './WorktreeNewDialog.vue'
import WorktreeEndDialog from './WorktreeEndDialog.vue'
import WorktreeContextMenu from './WorktreeContextMenu.vue'
import { mdiPlus, mdiChevronRight, mdiChevronDown, mdiRestore, mdiBroom } from '@mdi/js'
import type { ProjectWorkspace, Worktree } from '../types/pane'
import { useConfirm } from '../composables/useConfirm'

const { confirm } = useConfirm()

const props = defineProps<{
  workspace: ProjectWorkspace
}>()

const projectStore = useProjectStore()
const paneStore = usePaneStore()

// Active worktree's terminal-pane PTY session — where git commands from the
// sidebar Git menu get written. Using the default terminal (not the Claude
// pane) avoids typing git commands into an active Claude prompt.
const activeWorktree = computed(() =>
  props.workspace.worktrees.find(w => w.id === props.workspace.activeWorktreeId)
)
const activeTerminalSessionId = computed(() => {
  const wt = activeWorktree.value
  if (!wt) return null
  return paneStore.getPtySession(wt.defaultTerminalPaneId) ?? null
})
const activeBranchLabel = computed(() => activeWorktree.value?.branchName ?? '')

// Sort: main first, then alphabetical
const sortedWorktrees = computed(() => {
  return [...props.workspace.worktrees].sort((a, b) => {
    if (a.isMain && !b.isMain) return -1
    if (!a.isMain && b.isMain) return 1
    return a.branchName.localeCompare(b.branchName)
  })
})

const mainBranch = computed(() =>
  props.workspace.worktrees.find(w => w.isMain)?.branchName ?? 'main'
)

// Merge/PR actions ask Claude to do the work — only allowed when Claude
// is alive in the main worktree's terminal.
const mainClaudeRunning = computed(() => {
  const main = props.workspace.worktrees.find(w => w.isMain)
  if (!main) return false
  const s = projectStore.getClaudeStatus(main.id).status
  return s === 'ready' || s === 'working' || s === 'attention'
})

// ── New worktree dialog ─────────────────────────────────────────────────────

const showNewDialog = ref(false)

async function handleCreate(branchName: string, baseBranch: string | undefined) {
  await projectStore.addWorktree(props.workspace.id, branchName, baseBranch)
}

// ── End worktree dialog ─────────────────────────────────────────────────────

const endingWorktree = ref<{ id: string; branchName: string } | null>(null)

function openEndDialog(worktreeId: string, branchName: string) {
  endingWorktree.value = { id: worktreeId, branchName }
}

async function handleEnd(mode: 'delete' | 'merge' | 'discard' | 'pr') {
  if (!endingWorktree.value) return
  await projectStore.removeWorktree(props.workspace.id, endingWorktree.value.id, mode)
}

async function removeMerged(worktreeId: string) {
  try {
    await projectStore.removeMergedWorktree(props.workspace.id, worktreeId)
  } catch (e) {
    console.error(e)
  }
}

// ── Right-click context menu ────────────────────────────────────────────────

const ctxMenu = ref<{ x: number; y: number; worktree: Worktree } | null>(null)

function openContextMenu(event: MouseEvent, wt: Worktree) {
  ctxMenu.value = { x: event.clientX, y: event.clientY, worktree: wt }
}

function closeContextMenu() {
  ctxMenu.value = null
}

function canAskClaude(wt: Worktree): boolean {
  return projectStore.canAskClaudeToMerge(props.workspace.id, wt.id)
}

async function ctxManualMerge() {
  if (!ctxMenu.value) return
  const wt = ctxMenu.value.worktree
  closeContextMenu()
  try {
    await projectStore.manualMergeToParent(props.workspace.id, wt.id)
  } catch (e) {
    console.error('Manual merge failed:', e)
  }
}

async function ctxClaudeMerge() {
  if (!ctxMenu.value) return
  const wt = ctxMenu.value.worktree
  closeContextMenu()
  try {
    await projectStore.askClaudeToMerge(props.workspace.id, wt.id)
  } catch (e) {
    console.error('Ask Claude to merge failed:', e)
  }
}

async function ctxDeleteWorktree() {
  if (!ctxMenu.value) return
  const wt = ctxMenu.value.worktree
  closeContextMenu()
  const ok = await confirm({
    title: `Delete worktree ${wt.branchName}?`,
    message: 'Removes the worktree directory but keeps the branch.',
    confirmText: 'Delete',
  })
  if (!ok) return
  try {
    await projectStore.removeWorktree(props.workspace.id, wt.id, 'delete')
  } catch (e) {
    console.error('Delete worktree failed:', e)
  }
}

async function ctxDiscardWorktree() {
  if (!ctxMenu.value) return
  const wt = ctxMenu.value.worktree
  closeContextMenu()
  const ok = await confirm({
    title: `Discard worktree ${wt.branchName}?`,
    message: 'Force-removes the worktree, including any uncommitted changes. This cannot be undone.',
    confirmText: 'Discard',
    danger: true,
  })
  if (!ok) return
  try {
    await projectStore.removeWorktree(props.workspace.id, wt.id, 'discard')
  } catch (e) {
    console.error('Discard worktree failed:', e)
  }
}

async function ctxDismissMerged() {
  if (!ctxMenu.value) return
  const wt = ctxMenu.value.worktree
  closeContextMenu()
  try {
    await projectStore.removeMergedWorktree(props.workspace.id, wt.id)
  } catch (e) {
    console.error('Dismiss merged worktree failed:', e)
  }
}

// ── Stale worktrees ─────────────────────────────────────────────────────────

const staleEntries = computed(() => projectStore.getStaleWorktrees(props.workspace.id))
const staleExpanded = ref(false)

function folderName(path: string): string {
  return path.split(/[/\\]/).filter(Boolean).pop() ?? path
}

const restoreError = ref<string | null>(null)
async function restoreStale(path: string, branch: string | null) {
  if (!branch) return
  try {
    await projectStore.restoreStale(props.workspace.id, path, branch)
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e)
    restoreError.value = `Could not restore "${branch}": ${msg}`
    console.error('Restore stale worktree failed:', e)
  }
}

async function pruneStaleEntry(path: string, branch: string | null) {
  const label = branch ?? folderName(path)
  const ok = await confirm({
    title: `Prune stale worktree "${label}"?`,
    message: 'Removes the leftover .git/worktrees bookkeeping entry. The branch is kept; only the worktree registration is cleared.',
    confirmText: 'Prune',
  })
  if (!ok) return
  try {
    await projectStore.pruneStale(props.workspace.id, path)
  } catch (e) {
    console.error('Prune stale worktree failed:', e)
  }
}

async function pruneAllStale() {
  const n = staleEntries.value.length
  if (n === 0) return
  const ok = await confirm({
    title: `Clean up ${n} stale worktree${n === 1 ? '' : 's'}?`,
    message: 'Clears all .git/worktrees bookkeeping entries whose folders are gone. Branches are untouched.',
    confirmText: 'Clean up',
  })
  if (!ok) return
  try {
    await projectStore.pruneAllStale(props.workspace.id)
  } catch (e) {
    console.error('Prune all stale failed:', e)
  }
}
</script>

<template>
  <div class="worktree-panel">
    <div class="panel-header">
      <span class="panel-title">Worktrees</span>
      <button class="add-btn" title="New worktree" @click="showNewDialog = true">
        <MdiIcon :path="mdiPlus" :size="16" />
      </button>
    </div>

    <div class="worktree-list">
      <WorktreeCard
        v-for="wt in sortedWorktrees"
        :key="wt.id"
        :branch-name="wt.branchName"
        :is-main="wt.isMain"
        :is-active="wt.id === workspace.activeWorktreeId"
        :is-merged="projectStore.isMerged(wt.id)"
        :status="projectStore.getClaudeStatus(wt.id)"
        @click="projectStore.switchWorktree(workspace.id, wt.id)"
        @end="openEndDialog(wt.id, wt.branchName)"
        @remove="removeMerged(wt.id)"
        @contextmenu="(e) => openContextMenu(e, wt)"
      />

      <div v-if="staleEntries.length > 0" class="stale-section">
        <div class="stale-header">
          <button
            class="stale-toggle"
            :title="staleExpanded ? 'Collapse' : 'Expand'"
            @click="staleExpanded = !staleExpanded"
          >
            <MdiIcon :path="staleExpanded ? mdiChevronDown : mdiChevronRight" :size="14" />
            <span>Stale ({{ staleEntries.length }})</span>
          </button>
          <button class="stale-cleanup-all" title="Clean up all stale entries" @click="pruneAllStale">
            Clean up all
          </button>
        </div>
        <div v-if="staleExpanded" class="stale-list">
          <div v-for="entry in staleEntries" :key="entry.path" class="stale-row">
            <div class="stale-info">
              <div class="stale-branch">{{ entry.branch ?? '(detached)' }}</div>
              <div class="stale-path" :title="entry.path">{{ folderName(entry.path) }}</div>
            </div>
            <button
              class="stale-action"
              :disabled="!entry.branch"
              :title="entry.branch ? 'Restore worktree' : 'Cannot restore detached HEAD'"
              @click="restoreStale(entry.path, entry.branch)"
            >
              <MdiIcon :path="mdiRestore" :size="14" />
            </button>
            <button
              class="stale-action"
              title="Prune this stale entry"
              @click="pruneStaleEntry(entry.path, entry.branch)"
            >
              <MdiIcon :path="mdiBroom" :size="14" />
            </button>
          </div>
          <div v-if="restoreError" class="stale-error">
            {{ restoreError }}
            <button class="stale-error-dismiss" @click="restoreError = null">×</button>
          </div>
        </div>
      </div>
    </div>

    <!-- Git actions menu for the active worktree. Commands run in the
         worktree's terminal pane so they never land in a Claude prompt. -->
    <div v-if="activeWorktree" class="panel-footer">
      <GitMenu
        :session-id="activeTerminalSessionId"
        :label="activeBranchLabel"
        variant="full"
        open-direction="up"
      />
    </div>

    <WorktreeContextMenu
      v-if="ctxMenu"
      :worktree="ctxMenu.worktree"
      :click-x="ctxMenu.x"
      :click-y="ctxMenu.y"
      :is-merged="projectStore.isMerged(ctxMenu.worktree.id)"
      :can-ask-claude="canAskClaude(ctxMenu.worktree)"
      @close="closeContextMenu"
      @manual-merge="ctxManualMerge"
      @claude-merge="ctxClaudeMerge"
      @delete="ctxDeleteWorktree"
      @discard="ctxDiscardWorktree"
      @dismiss-merged="ctxDismissMerged"
    />

    <WorktreeNewDialog
      v-if="showNewDialog"
      :repo-root="workspace.repoRoot"
      :main-branch="mainBranch"
      :on-create="handleCreate"
      @close="showNewDialog = false"
    />

    <WorktreeEndDialog
      v-if="endingWorktree"
      :branch-name="endingWorktree.branchName"
      :main-branch="mainBranch"
      :main-claude-running="mainClaudeRunning"
      :on-end="handleEnd"
      @close="endingWorktree = null"
    />
  </div>
</template>

<style scoped>
.worktree-panel {
  width: 260px;
  min-width: 200px;
  display: flex;
  flex-direction: column;
  background: var(--color-bg-subtle);
  overflow: hidden;
  font-family: system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif;
}

.panel-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 8px 10px;
  border-bottom: 1px solid var(--color-card-border);
}

.panel-title {
  font-size: 12px;
  font-weight: 600;
  color: var(--color-text-secondary);
  text-transform: uppercase;
  letter-spacing: 0.5px;
}

.add-btn {
  background: none;
  border: none;
  color: var(--color-text-secondary);
  cursor: pointer;
  padding: 2px;
  border-radius: var(--radius-md);
}
.add-btn:hover {
  color: var(--color-text-primary);
  background: var(--color-bg-elevated);
}

.worktree-list {
  flex: 1;
  overflow-y: auto;
  padding: 6px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.panel-footer {
  padding: 8px 10px;
  border-top: 1px solid var(--color-card-border);
  background: var(--color-bg-subtle);
  flex-shrink: 0;
}

.stale-section {
  margin-top: 8px;
  padding-top: 6px;
  border-top: 1px dashed var(--color-card-border);
}

.stale-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 6px;
  padding: 2px 2px 2px 0;
}

.stale-toggle {
  display: flex;
  align-items: center;
  gap: 4px;
  background: none;
  border: none;
  color: var(--color-text-muted);
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.5px;
  padding: 2px 4px;
  border-radius: var(--radius-sm);
  cursor: pointer;
}
.stale-toggle:hover {
  color: var(--color-text-secondary);
  background: var(--color-bg-elevated);
}

.stale-cleanup-all {
  background: none;
  border: 1px solid var(--color-card-border);
  color: var(--color-text-muted);
  font-size: 11px;
  padding: 2px 8px;
  border-radius: var(--radius-sm);
  cursor: pointer;
}
.stale-cleanup-all:hover {
  color: var(--color-text-primary);
  border-color: var(--color-card-border-hover);
  background: var(--color-bg-elevated);
}

.stale-list {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin-top: 4px;
}

.stale-row {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 6px;
  border-radius: 4px;
  opacity: 0.75;
}
.stale-row:hover {
  background: var(--color-bg-elevated);
  opacity: 1;
}

.stale-info {
  flex: 1;
  min-width: 0;
}
.stale-branch {
  font-size: 12px;
  color: var(--color-text-secondary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.stale-path {
  font-size: 10px;
  color: var(--color-text-muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.stale-action {
  background: none;
  border: none;
  color: var(--color-text-muted);
  padding: 2px 4px;
  border-radius: var(--radius-sm);
  cursor: pointer;
  display: flex;
  align-items: center;
}
.stale-action:hover:not(:disabled) {
  color: var(--color-text-primary);
  background: var(--color-bg-elevated);
}
.stale-action:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}

.stale-error {
  margin-top: 4px;
  padding: 6px 8px;
  background: rgba(229, 83, 75, 0.12);
  border: 1px solid rgba(229, 83, 75, 0.4);
  border-radius: 4px;
  color: var(--color-text-secondary);
  font-size: 11px;
  display: flex;
  gap: 6px;
  align-items: flex-start;
}
.stale-error-dismiss {
  background: none;
  border: none;
  color: var(--color-text-muted);
  cursor: pointer;
  font-size: 14px;
  line-height: 1;
  padding: 0 2px;
}
.stale-error-dismiss:hover {
  color: var(--color-text-primary);
}
</style>
