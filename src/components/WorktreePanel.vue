<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount, nextTick } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { useProjectStore } from '../stores/project'
import WorktreeCard from './WorktreeCard.vue'
import MdiIcon from './MdiIcon.vue'
import { mdiPlus, mdiSourceMerge, mdiRobotOutline, mdiClose, mdiDice5Outline, mdiChevronDown, mdiDeleteOutline, mdiDeleteAlertOutline } from '@mdi/js'
import type { ProjectWorkspace, Worktree } from '../types/pane'
import { useConfirm } from '../composables/useConfirm'

const { confirm } = useConfirm()

// ── Random worktree name generator ──────────────────────────────────────────
const NAME_ADJECTIVES = [
  'swift', 'brave', 'clever', 'witty', 'lucky', 'mighty', 'silent', 'bold',
  'eager', 'fuzzy', 'jolly', 'nimble', 'quirky', 'sunny', 'wild', 'zesty',
  'cosmic', 'electric', 'frosty', 'golden', 'hidden', 'iron', 'lunar', 'misty',
]
const NAME_NOUNS = [
  'otter', 'falcon', 'panda', 'tiger', 'wolf', 'fox', 'lynx', 'hawk',
  'badger', 'beaver', 'cobra', 'dragon', 'eagle', 'gecko', 'heron', 'koala',
  'narwhal', 'octopus', 'penguin', 'raven', 'shark', 'turtle', 'viper', 'whale',
]
function randomWorktreeName(): string {
  const a = NAME_ADJECTIVES[Math.floor(Math.random() * NAME_ADJECTIVES.length)]
  const n = NAME_NOUNS[Math.floor(Math.random() * NAME_NOUNS.length)]
  return `${a}-${n}`
}

const props = defineProps<{
  workspace: ProjectWorkspace
}>()

const projectStore = useProjectStore()

// Sort: main first, then alphabetical
const sortedWorktrees = computed(() => {
  return [...props.workspace.worktrees].sort((a, b) => {
    if (a.isMain && !b.isMain) return -1
    if (!a.isMain && b.isMain) return 1
    return a.branchName.localeCompare(b.branchName)
  })
})

// ── New worktree dialog ─────────────────────────────────────────────────────

const showNewDialog = ref(false)
const newBranchName = ref('')
const newBaseBranch = ref('')
const creating = ref(false)
const createError = ref('')

const mainBranch = computed(() =>
  props.workspace.worktrees.find(w => w.isMain)?.branchName ?? 'main'
)

async function createWorktree() {
  if (!newBranchName.value.trim()) return
  creating.value = true
  createError.value = ''
  try {
    await projectStore.addWorktree(
      props.workspace.id,
      newBranchName.value.trim(),
      newBaseBranch.value.trim() || undefined
    )
    showNewDialog.value = false
    newBranchName.value = ''
    newBaseBranch.value = ''
  } catch (e: any) {
    createError.value = e?.message ?? String(e)
  } finally {
    creating.value = false
  }
}

async function openNewDialog() {
  newBranchName.value = randomWorktreeName()
  newBaseBranch.value = mainBranch.value
  createError.value = ''
  showNewDialog.value = true
  baseBranchSearch.value = ''
  baseDropdownOpen.value = false
  baseHighlight.value = 0
  // Load branch list and pick a sensible default that actually exists
  try {
    availableBranches.value = await invoke<string[]>('git_list_branches', { repoPath: props.workspace.repoRoot })
    if (!availableBranches.value.includes(newBaseBranch.value)) {
      // Cached "main branch" isn't a real ref — fall back to whichever variant exists
      const candidates = [
        mainBranch.value,
        `origin/${mainBranch.value}`,
        'main',
        'origin/main',
        'master',
        'origin/master',
      ]
      const pick = candidates.find(c => availableBranches.value.includes(c))
      newBaseBranch.value = pick ?? availableBranches.value[0] ?? ''
    }
  } catch (e) {
    console.error('Failed to list branches:', e)
    availableBranches.value = []
  }
}

function clearBranchName() {
  newBranchName.value = ''
  nextTick(() => {
    const input = document.querySelector('.dialog .branch-input') as HTMLInputElement | null
    input?.focus()
  })
}

function rerollBranchName() {
  newBranchName.value = randomWorktreeName()
}

// ── Searchable base-branch dropdown ─────────────────────────────────────────
const availableBranches = ref<string[]>([])
const baseDropdownOpen = ref(false)
const baseBranchSearch = ref('')
const baseHighlight = ref(0)

const filteredBranches = computed(() => {
  const q = baseBranchSearch.value.trim().toLowerCase()
  const list = availableBranches.value
  if (!q) return list
  return list.filter(b => b.toLowerCase().includes(q))
})

function openBaseDropdown() {
  baseDropdownOpen.value = true
  baseBranchSearch.value = ''
  baseHighlight.value = 0
  nextTick(() => {
    const input = document.querySelector('.base-dropdown-search') as HTMLInputElement | null
    input?.focus()
  })
}

function closeBaseDropdown() {
  baseDropdownOpen.value = false
}

function selectBaseBranch(name: string) {
  newBaseBranch.value = name
  baseDropdownOpen.value = false
}

function onBaseSearchKeydown(e: KeyboardEvent) {
  const list = filteredBranches.value
  if (e.key === 'ArrowDown') {
    e.preventDefault()
    baseHighlight.value = Math.min(baseHighlight.value + 1, list.length - 1)
  } else if (e.key === 'ArrowUp') {
    e.preventDefault()
    baseHighlight.value = Math.max(baseHighlight.value - 1, 0)
  } else if (e.key === 'Enter') {
    e.preventDefault()
    const pick = list[baseHighlight.value]
    if (pick) selectBaseBranch(pick)
  } else if (e.key === 'Escape') {
    e.preventDefault()
    closeBaseDropdown()
  }
}

function onDialogClickOutsideDropdown(e: MouseEvent) {
  if (!baseDropdownOpen.value) return
  const target = e.target as HTMLElement
  if (!target.closest('.base-branch-field')) closeBaseDropdown()
}

// ── End worktree dialog ─────────────────────────────────────────────────────

const showEndDialog = ref(false)
const endingWorktreeId = ref('')
const endingBranchName = ref('')
const ending = ref(false)
const endError = ref('')

function openEndDialog(worktreeId: string, branchName: string) {
  endingWorktreeId.value = worktreeId
  endingBranchName.value = branchName
  endError.value = ''
  showEndDialog.value = true
}

async function endWorktree(mode: 'delete' | 'merge' | 'discard' | 'pr') {
  ending.value = true
  endError.value = ''
  try {
    await projectStore.removeWorktree(props.workspace.id, endingWorktreeId.value, mode)
    showEndDialog.value = false
  } catch (e: any) {
    endError.value = e?.message ?? String(e)
  } finally {
    ending.value = false
  }
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
const ctxError = ref('')

function openContextMenu(event: MouseEvent, wt: Worktree) {
  ctxError.value = ''
  // Initial position at the click point — will be corrected after the menu renders
  ctxMenu.value = { x: event.clientX, y: event.clientY, worktree: wt }
  const anchorX = event.clientX
  const anchorY = event.clientY
  nextTick(() => {
    const el = document.querySelector('.worktree-context-menu') as HTMLElement | null
    if (!el || !ctxMenu.value) return
    const rect = el.getBoundingClientRect()
    const margin = 8
    const vw = window.innerWidth
    const vh = window.innerHeight

    // Horizontal: open right if it fits, otherwise flip left from the click point
    let x = anchorX
    if (anchorX + rect.width + margin > vw) {
      x = anchorX - rect.width
    }
    // Final clamp in case the menu is wider than the viewport
    x = Math.max(margin, Math.min(x, vw - rect.width - margin))

    // Vertical: open down if it fits, otherwise flip up from the click point
    let y = anchorY
    if (anchorY + rect.height + margin > vh) {
      y = anchorY - rect.height
    }
    y = Math.max(margin, Math.min(y, vh - rect.height - margin))

    ctxMenu.value = { x, y, worktree: wt }
  })
}

function closeContextMenu() {
  ctxMenu.value = null
}

function onWindowMouseDown(e: MouseEvent) {
  if (!ctxMenu.value) return
  const target = e.target as HTMLElement
  if (!target.closest('.worktree-context-menu')) closeContextMenu()
}

onMounted(() => document.addEventListener('mousedown', onWindowMouseDown))
onBeforeUnmount(() => document.removeEventListener('mousedown', onWindowMouseDown))

function canAskClaude(wt: Worktree): boolean {
  return projectStore.canAskClaudeToMerge(props.workspace.id, wt.id)
}

// Merge/PR actions ask Claude to do the work — only allowed when Claude
// is alive in the main worktree's terminal.
const mainClaudeRunning = computed(() => {
  const main = props.workspace.worktrees.find(w => w.isMain)
  if (!main) return false
  const s = projectStore.getClaudeStatus(main.id).status
  return s === 'ready' || s === 'working' || s === 'attention'
})

async function ctxManualMerge() {
  if (!ctxMenu.value) return
  const wt = ctxMenu.value.worktree
  closeContextMenu()
  try {
    await projectStore.manualMergeToParent(props.workspace.id, wt.id)
  } catch (e: any) {
    ctxError.value = e?.message ?? String(e)
    console.error('Manual merge failed:', e)
  }
}

async function ctxClaudeMerge() {
  if (!ctxMenu.value) return
  const wt = ctxMenu.value.worktree
  closeContextMenu()
  try {
    await projectStore.askClaudeToMerge(props.workspace.id, wt.id)
  } catch (e: any) {
    ctxError.value = e?.message ?? String(e)
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
  } catch (e: any) {
    ctxError.value = e?.message ?? String(e)
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
  } catch (e: any) {
    ctxError.value = e?.message ?? String(e)
    console.error('Discard worktree failed:', e)
  }
}

async function ctxDismissMerged() {
  if (!ctxMenu.value) return
  const wt = ctxMenu.value.worktree
  closeContextMenu()
  try {
    await projectStore.removeMergedWorktree(props.workspace.id, wt.id)
  } catch (e: any) {
    ctxError.value = e?.message ?? String(e)
    console.error('Dismiss merged worktree failed:', e)
  }
}
</script>

<template>
  <div class="worktree-panel">
    <div class="panel-header">
      <span class="panel-title">Worktrees</span>
      <button class="add-btn" title="New worktree" @click="openNewDialog">
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
    </div>

    <!-- Right-click context menu -->
    <Teleport to="body">
      <div
        v-if="ctxMenu"
        class="worktree-context-menu"
        :style="{ left: ctxMenu.x + 'px', top: ctxMenu.y + 'px' }"
      >
        <template v-if="projectStore.isMerged(ctxMenu.worktree.id)">
          <div class="ctx-section">
            <button class="ctx-item" @click="ctxDismissMerged">
              <MdiIcon :path="mdiClose" :size="14" />
              <span>Dismiss merged worktree</span>
            </button>
          </div>
        </template>
        <template v-else>
          <div v-if="ctxMenu.worktree.parentBranch" class="ctx-section">
            <button
              class="ctx-item"
              :disabled="!ctxMenu.worktree.parentBranch"
              @click="ctxManualMerge"
            >
              <MdiIcon :path="mdiSourceMerge" :size="14" />
              <span>Merge into <b>{{ ctxMenu.worktree.parentBranch }}</b></span>
            </button>
            <button
              class="ctx-item"
              :disabled="!canAskClaude(ctxMenu.worktree)"
              :title="!canAskClaude(ctxMenu.worktree) ? 'Parent worktree is busy or not open' : ''"
              @click="ctxClaudeMerge"
            >
              <MdiIcon :path="mdiRobotOutline" :size="14" />
              <span>Ask Claude to merge into <b>{{ ctxMenu.worktree.parentBranch }}</b></span>
            </button>
          </div>
          <div v-else class="ctx-empty">No parent branch recorded</div>
          <div class="ctx-section">
            <button class="ctx-item" @click="ctxDeleteWorktree">
              <MdiIcon :path="mdiDeleteOutline" :size="14" />
              <span>Delete worktree</span>
            </button>
            <button class="ctx-item ctx-danger" @click="ctxDiscardWorktree">
              <MdiIcon :path="mdiDeleteAlertOutline" :size="14" />
              <span>Discard changes</span>
            </button>
          </div>
        </template>
      </div>
    </Teleport>

    <!-- New worktree dialog -->
    <Teleport to="body">
      <div v-if="showNewDialog" class="dialog-overlay" @click.self="showNewDialog = false">
        <div class="dialog" @mousedown="onDialogClickOutsideDropdown">
          <h3>New Worktree</h3>
          <label>
            Branch name
            <div class="input-with-actions">
              <input
                v-model="newBranchName"
                class="branch-input"
                placeholder="feat/my-feature"
                autofocus
                @keydown.enter="createWorktree"
              />
              <button
                v-if="newBranchName"
                type="button"
                class="input-icon-btn"
                title="Clear"
                @click="clearBranchName"
              >
                <MdiIcon :path="mdiClose" :size="14" />
              </button>
              <button
                type="button"
                class="input-icon-btn"
                title="Random name"
                @click="rerollBranchName"
              >
                <MdiIcon :path="mdiDice5Outline" :size="14" />
              </button>
            </div>
          </label>
          <label class="base-branch-field">
            Base branch
            <button
              type="button"
              class="dropdown-trigger"
              @click="baseDropdownOpen ? closeBaseDropdown() : openBaseDropdown()"
            >
              <span class="dropdown-value">{{ newBaseBranch || mainBranch }}</span>
              <MdiIcon :path="mdiChevronDown" :size="14" />
            </button>
            <div v-if="baseDropdownOpen" class="dropdown-panel">
              <input
                v-model="baseBranchSearch"
                class="base-dropdown-search"
                placeholder="Search branches…"
                @keydown="onBaseSearchKeydown"
                @input="baseHighlight = 0"
              />
              <div class="dropdown-list">
                <div
                  v-for="(b, idx) in filteredBranches"
                  :key="b"
                  class="dropdown-item"
                  :class="{ active: idx === baseHighlight, selected: b === newBaseBranch }"
                  @mouseenter="baseHighlight = idx"
                  @click="selectBaseBranch(b)"
                >
                  {{ b }}
                </div>
                <div v-if="filteredBranches.length === 0" class="dropdown-empty">
                  No matching branches
                </div>
              </div>
            </div>
          </label>
          <div v-if="createError" class="error">{{ createError }}</div>
          <div class="dialog-actions">
            <button class="btn-secondary" @click="showNewDialog = false">Cancel</button>
            <button class="btn-primary" :disabled="!newBranchName.trim() || creating" @click="createWorktree">
              {{ creating ? 'Creating...' : 'Create' }}
            </button>
          </div>
        </div>
      </div>
    </Teleport>

    <!-- End worktree dialog -->
    <Teleport to="body">
      <div v-if="showEndDialog" class="dialog-overlay" @click.self="showEndDialog = false">
        <div class="dialog">
          <h3>End worktree: {{ endingBranchName }}</h3>
          <p class="dialog-desc">Choose how to handle this worktree:</p>
          <div v-if="endError" class="error">{{ endError }}</div>
          <div class="end-actions">
            <button class="btn-action" :disabled="ending" @click="endWorktree('delete')">
              Delete worktree
              <span class="btn-desc">Remove worktree, keep branch</span>
            </button>
            <button
              class="btn-action"
              :disabled="ending || !mainClaudeRunning"
              :title="!mainClaudeRunning ? 'Start Claude in the main terminal first' : ''"
              @click="endWorktree('merge')"
            >
              Merge &amp; delete
              <span class="btn-desc">Merge into {{ mainBranch }}, then remove</span>
            </button>
            <button
              class="btn-action"
              :disabled="ending || !mainClaudeRunning"
              :title="!mainClaudeRunning ? 'Start Claude in the main terminal first' : ''"
              @click="endWorktree('pr')"
            >
              Create PR &amp; delete
              <span class="btn-desc">Push, create PR, then remove</span>
            </button>
            <button class="btn-action btn-danger" :disabled="ending" @click="endWorktree('discard')">
              Discard changes
              <span class="btn-desc">Force remove, even with uncommitted changes</span>
            </button>
          </div>
          <div class="dialog-actions">
            <button class="btn-secondary" @click="showEndDialog = false">Cancel</button>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
.worktree-panel {
  width: 260px;
  min-width: 200px;
  display: flex;
  flex-direction: column;
  background: var(--color-bg);
  border-left: 1px solid var(--color-card-border);
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
  border-radius: 4px;
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

/* ── Dialogs ────────────────────────────────────────────────────────────── */

.dialog-overlay {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.5);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 1000;
}

.dialog {
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-card-border);
  border-radius: 8px;
  padding: 20px;
  min-width: 340px;
  max-width: 420px;
}

.dialog h3 {
  margin: 0 0 14px;
  font-size: 15px;
  color: var(--color-text-primary);
}

.dialog label {
  display: block;
  font-size: 12px;
  color: var(--color-text-secondary);
  margin-bottom: 10px;
}

.dialog input {
  display: block;
  width: 100%;
  margin-top: 4px;
  padding: 6px 8px;
  background: var(--color-bg);
  border: 1px solid var(--color-card-border);
  border-radius: 4px;
  color: var(--color-text-primary);
  font-size: 13px;
  outline: none;
  box-sizing: border-box;
}
.dialog input:focus {
  border-color: var(--azure);
}

.input-with-actions {
  position: relative;
  display: flex;
  align-items: center;
  margin-top: 4px;
}
.input-with-actions input {
  flex: 1;
  margin-top: 0;
  padding-right: 56px;
}
.input-icon-btn {
  position: absolute;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 22px;
  height: 22px;
  background: none;
  border: none;
  border-radius: 4px;
  color: var(--color-text-muted);
  cursor: pointer;
  padding: 0;
}
.input-icon-btn:hover {
  background: var(--color-bg-elevated);
  color: var(--color-text-primary);
}
.input-with-actions .input-icon-btn:nth-of-type(1) { right: 28px; }
.input-with-actions .input-icon-btn:nth-of-type(2) { right: 4px; }
.input-with-actions .input-icon-btn:only-of-type { right: 4px; }

.base-branch-field {
  position: relative;
}
.dropdown-trigger {
  display: flex;
  align-items: center;
  justify-content: space-between;
  width: 100%;
  margin-top: 4px;
  padding: 6px 8px;
  background: var(--color-bg);
  border: 1px solid var(--color-card-border);
  border-radius: 4px;
  color: var(--color-text-primary);
  font-size: 13px;
  font-family: inherit;
  cursor: pointer;
  text-align: left;
}
.dropdown-trigger:hover {
  border-color: var(--azure);
}
.dropdown-value {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.dropdown-panel {
  position: absolute;
  left: 0;
  right: 0;
  top: 100%;
  margin-top: 4px;
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-card-border);
  border-radius: 6px;
  box-shadow: 0 6px 18px rgba(0, 0, 0, 0.45);
  z-index: 10;
  overflow: hidden;
}
.base-dropdown-search {
  display: block;
  width: 100%;
  padding: 6px 8px;
  background: var(--color-bg);
  border: none;
  border-bottom: 1px solid var(--color-card-border);
  color: var(--color-text-primary);
  font-size: 12px;
  outline: none;
  box-sizing: border-box;
  margin-top: 0;
  border-radius: 0;
}
.dropdown-list {
  max-height: 200px;
  overflow-y: auto;
}
.dropdown-item {
  padding: 6px 10px;
  font-size: 12px;
  color: var(--color-text-primary);
  cursor: pointer;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.dropdown-item.active {
  background: rgba(255, 255, 255, 0.06);
}
.dropdown-item.selected {
  color: var(--azure);
  font-weight: 500;
}
.dropdown-empty {
  padding: 8px 10px;
  font-size: 12px;
  color: var(--color-text-muted);
  text-align: center;
}

.error {
  color: var(--color-danger);
  font-size: 12px;
  margin-bottom: 10px;
  padding: 6px 8px;
  background: rgba(239, 68, 68, 0.1);
  border-radius: 4px;
}

.dialog-actions {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
  margin-top: 14px;
}

.dialog-desc {
  font-size: 13px;
  color: var(--color-text-secondary);
  margin: 0 0 12px;
}

.end-actions {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.btn-primary, .btn-secondary, .btn-action {
  padding: 6px 14px;
  border-radius: 4px;
  font-size: 13px;
  cursor: pointer;
  border: 1px solid var(--color-card-border);
}

.btn-primary {
  background: var(--azure);
  color: white;
  border-color: var(--azure);
}
.btn-primary:disabled {
  opacity: 0.5;
  cursor: default;
}

.btn-secondary {
  background: var(--color-bg-subtle);
  color: var(--color-text-primary);
}

.btn-action {
  background: var(--color-bg-subtle);
  color: var(--color-text-primary);
  text-align: left;
  padding: 8px 12px;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.btn-action:hover {
  background: var(--color-bg-elevated);
  border-color: var(--azure);
}
.btn-action:disabled {
  opacity: 0.5;
  cursor: default;
}

.btn-danger {
  border-color: var(--color-danger);
  color: var(--color-danger);
}
.btn-danger:hover {
  background: rgba(239, 68, 68, 0.1);
}

.btn-desc {
  font-size: 11px;
  color: var(--color-text-muted);
}

/* ── Right-click context menu ─────────────────────────────────────────────── */

.worktree-context-menu {
  position: fixed;
  z-index: 2000;
  min-width: 240px;
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-card-border);
  border-radius: 6px;
  box-shadow: 0 6px 18px rgba(0, 0, 0, 0.45);
  padding: 4px 0;
  font-size: 12px;
  color: var(--color-text-primary);
}
.ctx-item {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  background: none;
  border: none;
  color: inherit;
  text-align: left;
  padding: 6px 12px;
  cursor: pointer;
  font: inherit;
}
.ctx-item:hover:not(:disabled) {
  background: rgba(255, 255, 255, 0.06);
}
.ctx-item:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}
.ctx-item.ctx-danger {
  color: var(--color-danger);
}
.ctx-section + .ctx-section {
  border-top: 1px solid var(--color-card-border);
  margin-top: 4px;
  padding-top: 4px;
}
.ctx-empty {
  padding: 8px 12px;
  color: var(--color-text-muted);
}
</style>
