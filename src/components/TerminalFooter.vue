<script setup lang="ts">
import { computed } from 'vue'
import MdiIcon from './MdiIcon.vue'
import GitMenu from './GitMenu.vue'
import type { GitInfo } from '../types/pane'
import {
  mdiSourceBranch,
  mdiFolderOutline,
  mdiRobotOutline,
  mdiDatabase,
  mdiArrowDown,
  mdiArrowUp,
  mdiCached,
  mdiBookOpenPageVariant,
  mdiAlertOutline,
  mdiCheckCircleOutline,
  mdiCircleEditOutline,
  mdiPlusCircleOutline,
} from '@mdi/js'

interface ClaudeSessionStatus {
  session_id: string
  model_id?: string | null
  input_tokens?: number | null
  output_tokens?: number | null
  cache_creation_input_tokens?: number | null
  cache_read_input_tokens?: number | null
  context_window_size?: number | null
  used_percentage?: number | null
  has_context?: boolean
  folder?: string | null
  branch?: string | null
}

const props = defineProps<{
  claudeRunning: boolean
  status: ClaudeSessionStatus | null
  folderName: string | null
  gitInfo: GitInfo | null
  sessionId: string | null
  // When true, the inline git actions menu is suppressed. Used in project
  // workspaces where git actions live in the worktree sidebar instead.
  hideGitMenu?: boolean
}>()

const emit = defineEmits<{ (e: 'rename-to-repo'): void }>()

// Clicking the folder segment renames the terminal to the repo name. Only
// actionable inside a git repo; the parent (TerminalPane) resolves the repo
// root and runs the confirm flow.
function onFolderClick() {
  if (props.gitInfo?.is_repo) emit('rename-to-repo')
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

// Context usage comes straight from Claude's statusLine capture (exact): used %
// (input-side) and the real window size (200k / 1M). Null until a capture lands.
const contextPct = computed(() => Math.min(100, Math.round(props.status?.used_percentage ?? 0)))

const contextMax = computed(() => {
  const w = props.status?.context_window_size ?? 0
  if (!w) return ''
  return w >= 1_000_000 ? `${w / 1_000_000}M` : `${w / 1000}k`
})

// Why a session might lack stats — shown on the warning icon for the punted
// edge cases (alias to an absolute claude path, claude started before this
// Arbiter version / outside Arbiter, or a login shell that wiped PATH).
const NO_STATS_REASON =
  "Context stats unavailable — this Claude session wasn't launched through Arbiter's wrapper " +
  "(e.g. a 'claude' alias pointing at an absolute path, or claude started outside Arbiter)."

function fmtK(n: number | null | undefined): string {
  if (n == null) return '0'
  // Truncate to one decimal (not round) so we match Claude's status line, which
  // formats via `bc scale=1` (e.g. 24450 → "24.4K", not "24.5K").
  if (n >= 1000) return (Math.floor(n / 100) / 10).toFixed(1) + 'K'
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

      <!-- Exact context + tokens from Claude's statusLine capture -->
      <template v-if="status.has_context">
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
      </template>

      <!-- No capture yet / non-intercepted session: warning icon only -->
      <template v-else>
        <span class="divider">|</span>
        <span class="seg" :title="NO_STATS_REASON">
          <MdiIcon :path="mdiAlertOutline" :size="12" class="icon-warn" />
        </span>
      </template>

      <span class="spacer" />

      <span
        v-if="status.folder"
        class="seg folder-seg"
        :class="{ clickable: gitInfo?.is_repo }"
        :role="gitInfo?.is_repo ? 'button' : undefined"
        :title="gitInfo?.is_repo ? 'Rename terminal to repo name' : undefined"
        @click="onFolderClick"
      >
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

    <!-- Not running Claude: compact git status on the LEFT; folder/branch stay right -->
    <template v-else>
      <span v-if="gitInfo?.is_repo" class="seg git-status">
        <span v-if="gitInfo.staged" class="git-staged" title="Staged">
          <MdiIcon :path="mdiCheckCircleOutline" :size="14" /><span class="git-num">{{ gitInfo.staged }}</span>
        </span>
        <span v-if="gitInfo.unstaged" class="git-unstaged" title="Modified (unstaged)">
          <MdiIcon :path="mdiCircleEditOutline" :size="14" /><span class="git-num">{{ gitInfo.unstaged }}</span>
        </span>
        <span v-if="gitInfo.untracked" class="git-untracked" title="Untracked">
          <MdiIcon :path="mdiPlusCircleOutline" :size="14" /><span class="git-num">{{ gitInfo.untracked }}</span>
        </span>
        <span v-if="gitInfo.ahead || gitInfo.behind" class="git-commits" title="Ahead / behind upstream">
          <span v-if="gitInfo.ahead" class="git-commit-grp"><MdiIcon :path="mdiArrowUp" :size="13" /><span class="git-num">{{ gitInfo.ahead }}</span></span>
          <span v-if="gitInfo.behind" class="git-commit-grp"><MdiIcon :path="mdiArrowDown" :size="13" /><span class="git-num">{{ gitInfo.behind }}</span></span>
        </span>
      </span>

      <span class="spacer" />

      <span
        class="seg folder-seg"
        :class="{ clickable: gitInfo?.is_repo }"
        :role="gitInfo?.is_repo ? 'button' : undefined"
        :title="gitInfo?.is_repo ? 'Rename terminal to repo name' : undefined"
        @click="onFolderClick"
      >
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

    <!-- Git actions menu (shown when in a repo, unless suppressed) -->
    <GitMenu v-if="gitInfo?.is_repo && !hideGitMenu" :session-id="sessionId" />
  </div>
</template>

<style scoped>
.terminal-footer {
  display: flex;
  align-items: center;
  gap: 6px;
  height: 26px;
  padding: 0 8px;
  background: var(--color-bg);
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
.icon-warn { color: #e5a03c; }
.context-val  { color: #569cd6; font-weight: 600; }
.context-max  { color: var(--color-text-muted); opacity: 0.6; font-weight: 400; }

.tok-seg { gap: 4px; }
.tok-in  { color: #4ec9b0; }
.tok-out { color: #c678dd; }
.tok-cw  { color: #569cd6; }
.tok-cr  { color: #d7ba7d; }

.folder-seg { gap: 4px; color: var(--color-text-muted); }
.folder { color: var(--color-text-primary); }
.folder-seg.clickable { cursor: pointer; border-radius: 3px; padding: 1px 3px; margin: 0 -3px; }
.folder-seg.clickable:hover { background: var(--color-card-border); }

.branch-seg { gap: 3px; }
.branch-icon { color: #F05032; }
.branch { color: #6a9955; font-weight: 600; }
.branch-bracket { color: var(--color-text-muted); opacity: 0.5; }

/* Compact working-tree status; zero-count groups are hidden in the template.
   Numbers stay at the footer's 11px; only the leading icons are larger. Icons
   use inline `vertical-align: middle` so each icon's centre lines up with the
   digit's optical middle (flex box-centering leaves digits looking high). */
.git-status { gap: 8px; font-weight: 600; }
.git-status > span { display: inline-flex; align-items: center; gap: 2px; }
.git-status svg { display: block; }
/* Flex centres the icon against the digit's line-box, but the numeral's mass
   sits high in that box — nudge the digit down to sit on the icon's centre. */
.git-num { display: block; transform: translateY(1px); }
.git-staged { color: #6a9955; }
.git-unstaged { color: #e5a03c; }
.git-untracked { color: #569cd6; }
.git-commits { color: var(--color-text-muted); gap: 5px; }
.git-commit-grp { display: inline-flex; align-items: center; gap: 1px; }

.waiting { font-style: italic; }
</style>
