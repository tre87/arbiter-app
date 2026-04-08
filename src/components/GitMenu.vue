<script setup lang="ts">
import { ref, nextTick, onMounted, onBeforeUnmount } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import MdiIcon from './MdiIcon.vue'
import {
  mdiSourceFork,
  mdiChevronUp,
  mdiSourceCommit,
  mdiArrowUpBold,
  mdiInformationOutline,
} from '@mdi/js'

const props = defineProps<{
  // PTY session to write git commands into.
  sessionId: string | null
  // Menu opens upward (bottom:100%) by default; set to 'down' for sidebar top anchor.
  openDirection?: 'up' | 'down'
  // Optional label shown next to the git icon (e.g. current branch).
  label?: string
  // Optional variant: 'compact' (footer style) or 'full' (sidebar style).
  variant?: 'compact' | 'full'
}>()

const menuOpen = ref(false)
const menuEl = ref<HTMLDivElement>()

// Commit message dialog
const commitDialogOpen = ref(false)
const commitMessage = ref('')
const commitAndPush = ref(false)
const commitScope = ref<'cwd' | 'repo'>('repo')
const commitInput = ref<HTMLInputElement>()

function toggleMenu() {
  menuOpen.value = !menuOpen.value
}

function onClickOutside(e: MouseEvent) {
  if (menuEl.value && !menuEl.value.contains(e.target as Node)) {
    menuOpen.value = false
  }
}

onMounted(() => document.addEventListener('mousedown', onClickOutside))
onBeforeUnmount(() => document.removeEventListener('mousedown', onClickOutside))

function writeToSession(cmd: string) {
  if (props.sessionId) {
    invoke('write_to_session', { sessionId: props.sessionId, data: cmd + '\r' })
  }
}

function openCommitDialog(andPush: boolean) {
  menuOpen.value = false
  commitMessage.value = ''
  commitAndPush.value = andPush
  commitScope.value = 'repo'
  commitDialogOpen.value = true
  nextTick(() => commitInput.value?.focus())
}

function submitCommit() {
  const msg = commitMessage.value.trim()
  if (!msg) return
  const escaped = msg.replace(/'/g, "'\\''")
  const addCmd = commitScope.value === 'repo' ? 'git add -A' : 'git add .'
  const commitCmd = `git commit -m '${escaped}'`
  const parts = [addCmd, commitCmd]
  if (commitAndPush.value) parts.push('git push')
  commitDialogOpen.value = false
  writeToSession(parts.join(' && '))
}

function cancelCommit() {
  commitDialogOpen.value = false
}

function gitStatus() {
  menuOpen.value = false
  writeToSession('git status')
}

function gitPush() {
  menuOpen.value = false
  writeToSession('git push')
}
</script>

<template>
  <div ref="menuEl" class="git-menu-anchor" :class="[variant ?? 'compact', 'open-' + (openDirection ?? 'up')]">
    <button class="git-btn" :class="{ active: menuOpen }" title="Git actions" @click.stop="toggleMenu">
      <MdiIcon :path="mdiSourceFork" :size="13" />
      <span v-if="label" class="git-label">{{ label }}</span>
      <MdiIcon :path="mdiChevronUp" :size="12" class="chevron" :class="{ flipped: menuOpen, down: openDirection === 'down' }" />
    </button>
    <div v-if="menuOpen" class="git-menu">
      <button class="git-menu-item" @click="gitStatus">
        <MdiIcon :path="mdiInformationOutline" :size="14" />
        <span>Status</span>
      </button>
      <button class="git-menu-item" @click="openCommitDialog(false)">
        <MdiIcon :path="mdiSourceCommit" :size="14" />
        <span>Commit</span>
      </button>
      <button class="git-menu-item" @click="gitPush">
        <MdiIcon :path="mdiArrowUpBold" :size="14" />
        <span>Push</span>
      </button>
      <button class="git-menu-item" @click="openCommitDialog(true)">
        <MdiIcon :path="mdiSourceCommit" :size="14" />
        <MdiIcon :path="mdiArrowUpBold" :size="14" class="combo-icon" />
        <span>Commit &amp; Push</span>
      </button>
    </div>
  </div>

  <!-- Commit message dialog -->
  <Teleport to="body">
    <div v-if="commitDialogOpen" class="commit-overlay" @mousedown.self="cancelCommit">
      <div class="commit-dialog">
        <h4 class="commit-title">{{ commitAndPush ? 'Commit & Push' : 'Commit' }}</h4>
        <input
          ref="commitInput"
          v-model="commitMessage"
          class="commit-input"
          placeholder="Commit message..."
          @keydown.enter="submitCommit"
          @keydown.escape="cancelCommit"
        />
        <div class="commit-scope">
          <label class="scope-option">
            <input type="radio" v-model="commitScope" value="repo" />
            <span>Entire repository</span>
          </label>
          <label class="scope-option">
            <input type="radio" v-model="commitScope" value="cwd" />
            <span>Current folder &amp; subfolders</span>
          </label>
        </div>
        <div class="commit-actions">
          <button class="commit-btn commit-btn-cancel" @click="cancelCommit">Cancel</button>
          <button class="commit-btn commit-btn-confirm" :disabled="!commitMessage.trim()" @click="submitCommit">
            {{ commitAndPush ? 'Commit & Push' : 'Commit' }}
          </button>
        </div>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
.git-menu-anchor {
  position: relative;
}

.git-btn {
  display: flex;
  align-items: center;
  gap: 4px;
  background: none;
  border: 1px solid var(--color-card-border);
  border-radius: 3px;
  color: var(--color-text-muted);
  cursor: pointer;
  padding: 2px 6px;
  line-height: 1;
  font: inherit;
  transition: color 0.15s, border-color 0.15s, background 0.15s;
}

.git-btn:hover,
.git-btn.active {
  background: var(--color-bg-elevated);
  color: var(--color-text-primary);
  border-color: #F05032;
}

.git-label {
  font-size: 11px;
  font-weight: 600;
  color: #6a9955;
  max-width: 140px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.chevron {
  transition: transform 0.15s;
}

.chevron.flipped {
  transform: rotate(180deg);
}

.chevron.down {
  transform: rotate(180deg);
}

.chevron.down.flipped {
  transform: rotate(0deg);
}

.git-menu {
  position: absolute;
  right: 0;
  z-index: 30;
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-card-border);
  border-radius: 5px;
  padding: 4px 0;
  min-width: 160px;
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
}

.open-up .git-menu {
  bottom: calc(100% + 4px);
}

.open-down .git-menu {
  top: calc(100% + 4px);
}

.full .git-btn {
  width: 100%;
  justify-content: center;
  padding: 6px 8px;
}

.full .git-menu {
  left: 0;
  right: 0;
}

.git-menu-item {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
  padding: 6px 12px;
  background: none;
  border: none;
  color: var(--color-text-primary);
  font-family: inherit;
  font-size: 11px;
  cursor: pointer;
  white-space: nowrap;
  transition: background 0.1s;
}

.git-menu-item:hover {
  background: rgba(255, 255, 255, 0.06);
}

.combo-icon {
  margin-left: -4px;
}

/* Commit dialog */
.commit-overlay {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.6);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 9999;
}

.commit-dialog {
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-card-border);
  border-radius: 8px;
  padding: 16px 20px;
  width: 400px;
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
}

.commit-title {
  margin: 0 0 12px;
  font-size: 13px;
  font-weight: 600;
  color: var(--color-text-primary);
}

.commit-input {
  width: 100%;
  padding: 8px 10px;
  background: var(--color-bg);
  border: 1px solid var(--color-card-border);
  border-radius: 4px;
  color: var(--color-text-primary);
  font-family: inherit;
  font-size: 12px;
  outline: none;
  box-sizing: border-box;
}

.commit-input:focus {
  border-color: var(--color-accent);
}

.commit-input::placeholder {
  color: var(--color-text-muted);
}

.commit-scope {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-top: 10px;
}

.scope-option {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  color: var(--color-text-secondary);
  cursor: pointer;
}

.scope-option input[type="radio"] {
  accent-color: var(--color-accent);
  cursor: pointer;
  margin: 0;
}

.commit-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 12px;
}

.commit-btn {
  padding: 6px 14px;
  border-radius: 4px;
  font-size: 12px;
  font-weight: 500;
  font-family: inherit;
  cursor: pointer;
  transition: background 0.15s, border-color 0.15s;
}

.commit-btn-cancel {
  background: var(--color-bg-subtle);
  color: var(--color-text-secondary);
  border: 1px solid var(--color-card-border);
}

.commit-btn-cancel:hover {
  background: var(--color-bg);
  color: var(--color-text-primary);
}

.commit-btn-confirm {
  background: var(--color-accent);
  color: #fff;
  border: 1px solid var(--color-accent);
}

.commit-btn-confirm:hover:not(:disabled) {
  background: var(--azure-deep);
  border-color: var(--azure-deep);
}

.commit-btn-confirm:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
