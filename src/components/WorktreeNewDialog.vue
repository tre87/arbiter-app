<script setup lang="ts">
import { ref, computed, nextTick, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import MdiIcon from './MdiIcon.vue'
import { mdiClose, mdiDice5Outline, mdiChevronDown } from '@mdi/js'

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
  repoRoot: string
  mainBranch: string
  onCreate: (branchName: string, baseBranch: string | undefined) => Promise<void>
}>()

const emit = defineEmits<{
  (e: 'close'): void
}>()

const newBranchName = ref('')
const newBaseBranch = ref('')
const creating = ref(false)
const createError = ref('')

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

async function init() {
  newBranchName.value = randomWorktreeName()
  newBaseBranch.value = props.mainBranch
  createError.value = ''
  baseBranchSearch.value = ''
  baseDropdownOpen.value = false
  baseHighlight.value = 0
  try {
    availableBranches.value = await invoke<string[]>('git_list_branches', { repoPath: props.repoRoot })
    if (!availableBranches.value.includes(newBaseBranch.value)) {
      const candidates = [
        props.mainBranch,
        `origin/${props.mainBranch}`,
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

// Re-init whenever the dialog is remounted (v-if-driven visibility)
watch(() => props.repoRoot, init, { immediate: true })

async function submit() {
  if (!newBranchName.value.trim()) return
  creating.value = true
  createError.value = ''
  try {
    await props.onCreate(newBranchName.value.trim(), newBaseBranch.value.trim() || undefined)
    emit('close')
  } catch (e: any) {
    createError.value = e?.message ?? String(e)
  } finally {
    creating.value = false
  }
}

function clearBranchName() {
  newBranchName.value = ''
  nextTick(() => {
    const input = document.querySelector('.worktree-new-dialog .branch-input') as HTMLInputElement | null
    input?.focus()
  })
}

function rerollBranchName() {
  newBranchName.value = randomWorktreeName()
}

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
</script>

<template>
  <Teleport to="body">
    <div class="dialog-overlay" @click.self="emit('close')">
      <div class="dialog worktree-new-dialog" @mousedown="onDialogClickOutsideDropdown">
        <h3>New Worktree</h3>
        <label>
          Branch name
          <div class="input-with-actions">
            <input
              v-model="newBranchName"
              class="branch-input"
              placeholder="feat/my-feature"
              autofocus
              @keydown.enter="submit"
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
          <button class="btn-secondary" @click="emit('close')">Cancel</button>
          <button class="btn-primary" :disabled="!newBranchName.trim() || creating" @click="submit">
            {{ creating ? 'Creating...' : 'Create' }}
          </button>
        </div>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
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
  border-radius: var(--radius-md);
  color: var(--color-text-primary);
  font-size: 13px;
  outline: none;
  box-sizing: border-box;
}
.dialog input:focus { border-color: var(--azure); }

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
  border-radius: var(--radius-md);
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

.base-branch-field { position: relative; }
.dropdown-trigger {
  display: flex;
  align-items: center;
  justify-content: space-between;
  width: 100%;
  margin-top: 4px;
  padding: 6px 8px;
  background: var(--color-bg);
  border: 1px solid var(--color-card-border);
  border-radius: var(--radius-md);
  color: var(--color-text-primary);
  font-size: 13px;
  font-family: inherit;
  cursor: pointer;
  text-align: left;
}
.dropdown-trigger:hover { border-color: var(--azure); }
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
.dropdown-item.active { background: rgba(255, 255, 255, 0.06); }
.dropdown-item.selected { color: var(--azure); font-weight: 500; }
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
  border-radius: var(--radius-md);
}

.dialog-actions {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
  margin-top: 14px;
}

.btn-primary, .btn-secondary {
  padding: 6px 14px;
  border-radius: var(--radius-md);
  font-size: 13px;
  cursor: pointer;
  border: 1px solid var(--color-card-border);
}
.btn-primary {
  background: var(--azure);
  color: white;
  border-color: var(--azure);
}
.btn-primary:disabled { opacity: 0.5; cursor: default; }
.btn-secondary {
  background: var(--color-bg-subtle);
  color: var(--color-text-primary);
}
</style>
