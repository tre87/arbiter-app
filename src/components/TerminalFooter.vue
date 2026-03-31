<script setup lang="ts">
import { ref, computed, nextTick, onMounted, onBeforeUnmount } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import MdiIcon from './MdiIcon.vue'
import {
  mdiSourceBranch,
  mdiFolderOutline,
  mdiSourceCommit,
  mdiArrowUpBold,
  mdiSourceFork,
  mdiChevronUp,
  mdiRobotOutline,
  mdiDatabase,
  mdiArrowDown,
  mdiArrowUp,
  mdiCached,
  mdiBookOpenPageVariant,
  mdiInformationOutline,
} from '@mdi/js'

interface ClaudeSessionStatus {
  session_id: string
  model_id?: string | null
  input_tokens?: number | null
  output_tokens?: number | null
  cache_creation_input_tokens?: number | null
  cache_read_input_tokens?: number | null
  folder?: string | null
  branch?: string | null
}

const props = defineProps<{
  claudeRunning: boolean
  status: ClaudeSessionStatus | null
  folderName: string | null
  gitInfo: { is_repo: boolean; branch: string | null } | null
  sessionId: string | null
}>()

const menuOpen = ref(false)
const menuEl = ref<HTMLDivElement>()

// Commit message dialog
const commitDialogOpen = ref(false)
const commitMessage = ref('')
const commitAndPush = ref(false)
const commitScope = ref<'cwd' | 'repo'>('cwd')
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
  // Escape single quotes in the message
  const escaped = msg.replace(/'/g, "'\\''")
  const addCmd = commitScope.value === 'repo'
    ? 'git add -A'
    : 'git add .'
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

function modelLabel(id: string | null | undefined): { name: string; cls: string } {
  if (!id) return { name: '', cls: '' }
  const m = id.match(/(opus|sonnet|haiku|flash)[- ]?(\d+)[- ]?(\d+)?/)
  if (m) {
    const family = m[1].charAt(0).toUpperCase() + m[1].slice(1)
    const ver = m[3] ? `${m[2]}.${m[3]}` : m[2]
    return { name: `${family} ${ver}`, cls: m[1] }
  }
  return { name: id.replace('claude-', ''), cls: '' }
}

function contextWindow(id: string | null | undefined): number {
  if (!id) return 200_000
  if (id.includes('haiku')) return 200_000
  if (id.includes('opus')) return 200_000
  if (id.includes('sonnet')) return 200_000
  return 200_000
}

const totalTokens = computed(() => {
  if (!props.status) return 0
  return (props.status.input_tokens ?? 0)
    + (props.status.output_tokens ?? 0)
    + (props.status.cache_creation_input_tokens ?? 0)
    + (props.status.cache_read_input_tokens ?? 0)
})

const contextPct = computed(() => {
  const max = contextWindow(props.status?.model_id)
  if (max === 0) return 0
  return Math.min(100, Math.round((totalTokens.value / max) * 100))
})

const contextMax = computed(() => {
  const max = contextWindow(props.status?.model_id)
  return (max / 1000) + 'k'
})

function fmtK(n: number | null | undefined): string {
  if (n == null) return '0'
  if (n >= 1000) return (n / 1000).toFixed(1) + 'K'
  return String(n)
}
</script>

<template>
  <div class="terminal-footer">
    <!-- Claude running mode -->
    <template v-if="claudeRunning && status">
      <span v-if="modelLabel(status.model_id).name" class="seg" title="Model">
        <MdiIcon :path="mdiRobotOutline" :size="12" :class="'icon-' + modelLabel(status.model_id).cls" />
        <span :class="['model', 'model-' + modelLabel(status.model_id).cls]">{{ modelLabel(status.model_id).name }}</span>
      </span>

      <span class="divider">|</span>

      <span class="seg" title="Context">
        <MdiIcon :path="mdiDatabase" :size="12" class="icon-context" />
        <span class="context-val">{{ contextPct }}%<span class="context-max">/{{ contextMax }}</span></span>
      </span>

      <span class="divider">|</span>

      <span class="seg tok-seg">
        <MdiIcon :path="mdiArrowDown" :size="11" class="tok-in" title="Input tokens" />
        <span class="tok-in">{{ fmtK(status.input_tokens) }}</span>
        <MdiIcon :path="mdiArrowUp" :size="11" class="tok-out" title="Output tokens" />
        <span class="tok-out">{{ fmtK(status.output_tokens) }}</span>
        <MdiIcon :path="mdiCached" :size="11" class="tok-cw" title="Cache write tokens" />
        <span class="tok-cw">{{ fmtK(status.cache_creation_input_tokens) }}</span>
        <MdiIcon :path="mdiBookOpenPageVariant" :size="11" class="tok-cr" title="Cache read tokens" />
        <span class="tok-cr">{{ fmtK(status.cache_read_input_tokens) }}</span>
      </span>

      <span class="spacer" />

      <span v-if="status.folder" class="seg folder-seg">
        <MdiIcon :path="mdiFolderOutline" :size="12" />
        <span class="folder">{{ status.folder }}</span>
      </span>

      <template v-if="status.branch">
        <span class="divider">|</span>
        <span class="seg branch-seg">
          <MdiIcon :path="mdiSourceBranch" :size="13" class="branch-icon" />
          <span class="branch">{{ status.branch }}</span>
        </span>
      </template>
    </template>

    <!-- Claude running but no status yet -->
    <template v-else-if="claudeRunning && !status">
      <span class="lbl waiting">waiting for first turn…</span>
      <span class="spacer" />
    </template>

    <!-- Not running Claude: show folder/git info right-aligned -->
    <template v-else>
      <span class="spacer" />
      <span class="seg folder-seg">
        <MdiIcon :path="mdiFolderOutline" :size="12" />
        <span class="folder">{{ folderName }}</span>
        <template v-if="gitInfo?.branch">
          <span class="branch-bracket">[</span>
          <MdiIcon :path="mdiSourceBranch" :size="12" class="branch-icon" />
          <span class="branch">{{ gitInfo.branch }}</span>
          <span class="branch-bracket">]</span>
        </template>
      </span>
    </template>

    <!-- Git actions menu (shown in both modes when in a repo) -->
    <div v-if="gitInfo?.is_repo" ref="menuEl" class="git-menu-anchor">
      <button class="git-btn" :class="{ active: menuOpen }" title="Git actions" @click.stop="toggleMenu">
        <MdiIcon :path="mdiSourceFork" :size="13" />
        <MdiIcon :path="mdiChevronUp" :size="12" class="chevron" :class="{ flipped: menuOpen }" />
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
          <span>Commit & Push</span>
        </button>
      </div>
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
            <span>Current folder & subfolders</span>
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
.terminal-footer {
  display: flex;
  align-items: center;
  gap: 6px;
  height: 26px;
  padding: 0 8px;
  background: var(--color-bg-subtle);
  border-top: 1px solid var(--color-card-border);
  flex-shrink: 0;
  overflow: visible;
  font-family: Consolas, 'Cascadia Code', Menlo, 'SF Mono', monospace;
  font-size: 11px;
  user-select: none;
  position: relative;
}

.seg {
  display: flex;
  align-items: center;
  gap: 3px;
  white-space: nowrap;
}

.lbl {
  color: var(--color-text-muted);
  opacity: 0.6;
}

.divider {
  color: var(--color-card-border);
  flex-shrink: 0;
}

.spacer { flex: 1; }

.model        { font-weight: 600; color: var(--color-text-primary); }
.model-sonnet { color: #9cdcfe; }
.model-opus   { color: #4ec9b0; }
.model-haiku  { color: #b5cea8; }
.model-flash  { color: #c678dd; }

.icon-sonnet { color: #9cdcfe; }
.icon-opus   { color: #4ec9b0; }
.icon-haiku  { color: #b5cea8; }
.icon-flash  { color: #c678dd; }

.icon-context { color: #569cd6; }
.context-val  { color: #569cd6; font-weight: 600; }
.context-max  { color: var(--color-text-muted); opacity: 0.6; font-weight: 400; }

.tok-seg { gap: 4px; }
.tok-in  { color: #4ec9b0; }
.tok-out { color: #c678dd; }
.tok-cw  { color: #569cd6; }
.tok-cr  { color: #d7ba7d; }

.folder-seg { gap: 4px; color: var(--color-text-muted); }
.folder { color: var(--color-text-primary); }

.branch-seg { gap: 3px; }
.branch-icon { color: #F05032; }
.branch { color: #6a9955; font-weight: 600; }
.branch-bracket { color: var(--color-text-muted); opacity: 0.5; }

.waiting { font-style: italic; }

/* Git actions */
.git-menu-anchor {
  position: relative;
}

.git-btn {
  display: flex;
  align-items: center;
  gap: 2px;
  background: none;
  border: 1px solid var(--color-card-border);
  border-radius: 3px;
  color: var(--color-text-muted);
  cursor: pointer;
  padding: 2px 5px;
  line-height: 1;
  transition: color 0.15s, border-color 0.15s, background 0.15s;
}

.git-btn:hover,
.git-btn.active {
  background: var(--color-bg-elevated);
  color: var(--color-text-primary);
  border-color: #F05032;
}

.chevron {
  transition: transform 0.15s;
}

.chevron.flipped {
  transform: rotate(180deg);
}

.git-menu {
  position: absolute;
  bottom: calc(100% + 4px);
  right: 0;
  z-index: 30;
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-card-border);
  border-radius: 5px;
  padding: 4px 0;
  min-width: 160px;
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
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
