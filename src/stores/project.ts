import { defineStore } from 'pinia'
import { ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { usePaneStore } from './pane'

// ── Types ────────────────────────────────────────────────────────────────────

export interface WorktreeInfo {
  path: string
  branch: string | null
  head: string | null
  is_main: boolean
}

export interface DirEntry {
  name: string
  path: string
  is_dir: boolean
  is_symlink: boolean
}

export interface WorktreeClaudeStatus {
  model: string | null
  inputTokens: number
  outputTokens: number
  cacheReadTokens: number
  cacheWriteTokens: number
  contextPercent: number
  status: 'ready' | 'working' | 'attention' | 'exited'
  sessionId: string | null
}

// ── Store ────────────────────────────────────────────────────────────────────

export const useProjectStore = defineStore('project', () => {
  // Lazy access to avoid circular init — paneStore is only used inside functions
  const getPaneStore = () => usePaneStore()

  // ── Explorer state per worktree ───────────────────────────────────────────

  // worktreeId → path → DirEntry[]
  const directoryCache = ref<Record<string, Record<string, DirEntry[]>>>({})
  // worktreeId → relative_path → status string
  const gitStatusCache = ref<Record<string, Record<string, string>>>({})
  // worktreeId → watcher IDs
  const activeWatchers = ref<Record<string, string[]>>({})

  // ── Merged-state tracking (worktreeId → true) ────────────────────────────
  // Set by the refs watcher when a non-main worktree's branch becomes
  // fully merged into its parent branch. Drives the disabled UI + merge badge.
  const mergedWorktrees = ref<Record<string, boolean>>({})

  function isMerged(worktreeId: string): boolean {
    return !!mergedWorktrees.value[worktreeId]
  }

  // ── Refs watchers (one per project workspace) ────────────────────────────
  // Watches `<repoRoot>/.git/refs/heads/` recursively. Any branch tip update
  // (merge, push, fetch) triggers checkMergedAll for that workspace.
  const refsWatchers = ref<Record<string, { watcherId: string; unlisten: () => void }>>({})

  // ── Claude status per worktree ────────────────────────────────────────────

  const claudeStatuses = ref<Record<string, WorktreeClaudeStatus>>({})

  function getClaudeStatus(worktreeId: string): WorktreeClaudeStatus {
    return claudeStatuses.value[worktreeId] ?? {
      model: null,
      inputTokens: 0,
      outputTokens: 0,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
      contextPercent: 0,
      status: 'exited',
      sessionId: null,
    }
  }

  function updateClaudeStatus(worktreeId: string, update: Partial<WorktreeClaudeStatus>) {
    const current = getClaudeStatus(worktreeId)
    claudeStatuses.value[worktreeId] = { ...current, ...update }
  }

  // Map claudePaneId → worktreeId for event routing
  const paneToWorktree = ref<Record<string, string>>({})

  function registerPaneWorktree(claudePaneId: string, worktreeId: string) {
    paneToWorktree.value[claudePaneId] = worktreeId
  }

  // ── Reactive derivation from centralized claudePaneStates ────────────────
  // Instead of duplicating event listeners from TerminalPane, we reactively
  // watch the centralized ClaudePaneState in the pane store and derive the
  // worktree card status from it. Single source of truth.
  watch(
    () => {
      const paneStore = getPaneStore()
      const snap: Record<string, { lifecycle: string; model: string | null; inputTokens: number; outputTokens: number; cacheReadTokens: number; cacheWriteTokens: number; contextPercent: number; sessionId: string | null } | null> = {}
      for (const [paneId, wtId] of Object.entries(paneToWorktree.value)) {
        const state = paneStore.getClaudePaneState(paneId)
        snap[wtId] = state
      }
      return snap
    },
    (snap) => {
      for (const [wtId, state] of Object.entries(snap)) {
        if (!state || state.lifecycle === 'closed') {
          updateClaudeStatus(wtId, { status: 'exited' })
        } else {
          const status = state.lifecycle === 'launching' ? 'ready' : state.lifecycle as WorktreeClaudeStatus['status']
          const update: Partial<WorktreeClaudeStatus> = {
            status,
            inputTokens: state.inputTokens,
            outputTokens: state.outputTokens,
            cacheReadTokens: state.cacheReadTokens,
            cacheWriteTokens: state.cacheWriteTokens,
            contextPercent: state.contextPercent,
          }
          if (state.model) update.model = state.model
          if (state.sessionId) update.sessionId = state.sessionId
          updateClaudeStatus(wtId, update)
        }
      }
    },
    { deep: true }
  )

  function getWorktreeIdForPane(claudePaneId: string): string | undefined {
    // Check map first (populated on create)
    const mapped = paneToWorktree.value[claudePaneId]
    if (mapped) return mapped
    // Fallback: scan all project workspaces (needed after restore)
    const paneStore = getPaneStore()
    for (const ws of paneStore.workspaces) {
      if (ws.type !== 'project') continue
      const wt = ws.worktrees.find(w => w.claudePaneId === claudePaneId)
      if (wt) {
        paneToWorktree.value[claudePaneId] = wt.id
        return wt.id
      }
    }
    return undefined
  }

  // ── Explorer operations ───────────────────────────────────────────────────

  async function loadDirectory(worktreeId: string, dirPath: string): Promise<DirEntry[]> {
    try {
      const entries = await invoke<DirEntry[]>('read_directory', { path: dirPath })
      if (!directoryCache.value[worktreeId]) {
        directoryCache.value[worktreeId] = {}
      }
      directoryCache.value[worktreeId][dirPath] = entries
      return entries
    } catch (e) {
      console.error('Failed to load directory:', e)
      return []
    }
  }

  function getCachedDirectory(worktreeId: string, dirPath: string): DirEntry[] | undefined {
    return directoryCache.value[worktreeId]?.[dirPath]
  }

  async function refreshGitStatus(worktreeId: string, worktreePath: string): Promise<void> {
    try {
      const statuses = await invoke<Record<string, string>>('git_file_status', {
        repoRoot: worktreePath,
        worktreePath,
      })
      gitStatusCache.value[worktreeId] = statuses
    } catch (e) {
      console.error('Failed to refresh git status:', e)
    }
  }

  function getFileStatus(worktreeId: string, relativePath: string): string | undefined {
    return gitStatusCache.value[worktreeId]?.[relativePath]
  }

  function getFolderStatus(worktreeId: string, relativePath: string): string | undefined {
    // Propagate: if any file under this folder has a status, the folder inherits it
    // Priority: conflicted > modified > added > untracked > deleted
    const statuses = gitStatusCache.value[worktreeId]
    if (!statuses) return undefined

    const prefix = relativePath.endsWith('/') ? relativePath : relativePath + '/'
    const priority: Record<string, number> = {
      conflicted: 5,
      modified: 4,
      added: 3,
      untracked: 2,
      deleted: 1,
      renamed: 3,
    }

    let highest: string | undefined
    let highestPriority = 0

    for (const [path, status] of Object.entries(statuses)) {
      if (path.startsWith(prefix) || path === relativePath) {
        const p = priority[status] ?? 0
        if (p > highestPriority) {
          highestPriority = p
          highest = status
        }
      }
    }
    return highest
  }

  // ── File watcher management ───────────────────────────────────────────────

  async function watchDirectory(worktreeId: string, dirPath: string): Promise<string | null> {
    try {
      const watcherId = await invoke<string>('watch_directory', { path: dirPath, recursive: false })
      if (!activeWatchers.value[worktreeId]) {
        activeWatchers.value[worktreeId] = []
      }
      activeWatchers.value[worktreeId].push(watcherId)
      return watcherId
    } catch (e) {
      console.error('Failed to watch directory:', e)
      return null
    }
  }

  async function unwatchAll(worktreeId: string): Promise<void> {
    const watchers = activeWatchers.value[worktreeId] ?? []
    for (const id of watchers) {
      try {
        await invoke('unwatch_directory', { watcherId: id })
      } catch { /* ignore */ }
    }
    delete activeWatchers.value[worktreeId]
  }

  // ── Merged-state detection ───────────────────────────────────────────────

  // Recheck every non-main worktree in the workspace. If a branch has become
  // an ancestor of its parent branch, mark it merged. If the active worktree
  // is the one that just got merged, switch to the parent's worktree.
  async function checkMergedAll(workspaceId: string): Promise<void> {
    const ws = getPaneStore().getProjectWorkspace(workspaceId)
    if (!ws) return

    let switched = false
    for (const wt of ws.worktrees) {
      if (wt.isMain || !wt.parentBranch) continue
      // Already marked — skip; only the explicit remove path clears it.
      if (mergedWorktrees.value[wt.id]) continue

      try {
        const merged = await invoke<boolean>('git_is_branch_merged', {
          repoRoot: ws.repoRoot,
          branch: wt.branchName,
          intoBranch: wt.parentBranch,
        })
        if (merged) {
          mergedWorktrees.value[wt.id] = true
          // If active, switch to the parent worktree (or main as fallback)
          if (!switched && ws.activeWorktreeId === wt.id) {
            const parentWt = ws.worktrees.find(w => w.branchName === wt.parentBranch)
              ?? ws.worktrees.find(w => w.isMain)
            if (parentWt) {
              getPaneStore().switchWorktree(workspaceId, parentWt.id)
              switched = true
            }
          }
        }
      } catch (e) {
        console.error('git_is_branch_merged failed for', wt.branchName, e)
      }
    }
  }

  async function setupRefsWatcher(workspaceId: string): Promise<void> {
    const ws = getPaneStore().getProjectWorkspace(workspaceId)
    if (!ws) return
    if (refsWatchers.value[workspaceId]) return // already set up

    // Main worktree's `.git` is a directory containing refs/heads/.
    // Non-main worktrees have a `.git` *file*, but they all share the same
    // refs store under the main worktree's `.git`.
    const mainWt = ws.worktrees.find(w => w.isMain)
    if (!mainWt) return

    const refsHeadsPath = `${mainWt.path}/.git/refs/heads`

    try {
      const watcherId = await invoke<string>('watch_directory', { path: refsHeadsPath, recursive: true })

      // Debounce so a flurry of ref updates collapses into one check.
      let debounce: ReturnType<typeof setTimeout> | null = null
      const unlisten = await listen(`fs-changed-${watcherId}`, () => {
        if (debounce) clearTimeout(debounce)
        debounce = setTimeout(() => { checkMergedAll(workspaceId) }, 300)
      }) as unknown as (() => void)

      refsWatchers.value[workspaceId] = { watcherId, unlisten }

      // Initial check on startup so already-merged branches show up immediately.
      checkMergedAll(workspaceId)
    } catch (e) {
      console.error('Failed to set up refs watcher:', e)
    }
  }

  async function teardownRefsWatcher(workspaceId: string): Promise<void> {
    const w = refsWatchers.value[workspaceId]
    if (!w) return
    try { w.unlisten() } catch { /* ignore */ }
    try { await invoke('unwatch_directory', { watcherId: w.watcherId }) } catch { /* ignore */ }
    delete refsWatchers.value[workspaceId]
  }

  // ── Merge actions ────────────────────────────────────────────────────────

  // Manually merge a worktree's branch into its parent branch.
  // Runs `git merge` in the parent worktree's directory. The refs watcher
  // will then mark this worktree as merged automatically.
  async function manualMergeToParent(workspaceId: string, worktreeId: string): Promise<void> {
    const ws = getPaneStore().getProjectWorkspace(workspaceId)
    if (!ws) return
    const wt = ws.worktrees.find(w => w.id === worktreeId)
    if (!wt || !wt.parentBranch) throw new Error('No parent branch recorded for this worktree')

    await invoke('git_merge_branch', {
      repoRoot: ws.repoRoot,
      sourceBranch: wt.branchName,
      targetBranch: wt.parentBranch,
    })
    // Optimistic: re-run the merged check immediately so the UI doesn't wait
    // for the watcher debounce.
    await checkMergedAll(workspaceId)
  }

  // Ask the parent worktree's Claude to perform the merge by writing a prompt
  // into its PTY session. Caller is responsible for checking that the parent
  // Claude is not currently working.
  async function askClaudeToMerge(workspaceId: string, worktreeId: string): Promise<void> {
    const ws = getPaneStore().getProjectWorkspace(workspaceId)
    if (!ws) return
    const wt = ws.worktrees.find(w => w.id === worktreeId)
    if (!wt || !wt.parentBranch) throw new Error('No parent branch recorded for this worktree')

    const parentWt = ws.worktrees.find(w => w.branchName === wt.parentBranch)
    if (!parentWt) throw new Error(`Parent worktree "${wt.parentBranch}" is not open in Arbiter`)

    const parentStatus = getClaudeStatus(parentWt.id).status
    if (parentStatus === 'working') {
      throw new Error(`Parent worktree's Claude is currently working — wait for it to finish`)
    }

    const sessionId = getPaneStore().getPtySession(parentWt.claudePaneId)
    if (!sessionId) throw new Error('Parent worktree has no active Claude session')

    const prompt =
      `Please merge the branch "${wt.branchName}" into "${wt.parentBranch}". ` +
      `Run the merge from this worktree (which is on "${wt.parentBranch}"). ` +
      `If there are conflicts, stop and report them — do not auto-resolve.`

    // Send as a single line + Enter so Claude picks it up as a turn.
    await invoke('write_to_session', { sessionId, data: prompt + '\r' })
  }

  // Returns whether the parent worktree's Claude is free to take a merge prompt.
  function canAskClaudeToMerge(workspaceId: string, worktreeId: string): boolean {
    const ws = getPaneStore().getProjectWorkspace(workspaceId)
    if (!ws) return false
    const wt = ws.worktrees.find(w => w.id === worktreeId)
    if (!wt || !wt.parentBranch) return false
    const parentWt = ws.worktrees.find(w => w.branchName === wt.parentBranch)
    if (!parentWt) return false
    const s = getClaudeStatus(parentWt.id).status
    return s === 'ready' || s === 'attention'
  }

  // ── Worktree lifecycle ────────────────────────────────────────────────────

  // Git on Windows returns forward-slash paths (`C:/Users/...`); the shell,
  // PTY, and file-watcher pipeline behave more predictably with native
  // backslashes. Normalize once at the boundary.
  const isWindows = navigator.platform.startsWith('Win')
  function normalizePath(p: string): string {
    return isWindows ? p.replace(/\//g, '\\') : p
  }

  async function createProjectWorkspace(repoRoot: string): Promise<string | null> {
    try {
      const normalizedRoot = normalizePath(repoRoot)
      // Detect main branch
      const worktrees = await invoke<WorktreeInfo[]>('git_worktree_list', { repoRoot: normalizedRoot })
      const mainWt = worktrees.find(wt => wt.is_main) ?? worktrees[0]
      if (!mainWt) return null

      const mainPath = normalizePath(mainWt.path)
      const branchName = mainWt.branch ?? 'main'
      const repoName = normalizedRoot.split(/[/\\]/).pop() ?? 'Project'

      const result = getPaneStore().addProjectWorkspace(repoName, normalizedRoot, branchName, mainPath)
      registerPaneWorktree(result.claudePaneId, result.worktreeId)
      updateClaudeStatus(result.worktreeId, { status: 'ready' })

      // Pre-populate model from project settings if available
      try {
        const model = await invoke<string | null>('get_project_model', { projectPath: mainPath })
        if (model) updateClaudeStatus(result.worktreeId, { model })
      } catch { /* no settings file */ }

      // Start watching .git/refs/heads for merge detection
      setupRefsWatcher(result.workspaceId)

      return result.workspaceId
    } catch (e) {
      console.error('Failed to create project workspace:', e)
      return null
    }
  }

  async function addWorktree(workspaceId: string, branchName: string, baseBranch?: string): Promise<string | null> {
    const ws = getPaneStore().getProjectWorkspace(workspaceId)
    if (!ws) return null

    // Determine the parent branch for merge tracking. Falls back to the
    // active worktree's branch (which is what `git worktree add` uses by
    // default when no explicit base is given).
    const activeWt = ws.worktrees.find(w => w.id === ws.activeWorktreeId)
    const parentBranch = baseBranch
      ?? activeWt?.branchName
      ?? ws.worktrees.find(w => w.isMain)?.branchName
      ?? null

    try {
      const info = await invoke<WorktreeInfo>('git_worktree_add', {
        repoRoot: ws.repoRoot,
        branchName,
        baseBranch: baseBranch ?? null,
      })

      const infoPath = normalizePath(info.path)
      const result = getPaneStore().addWorktreeToProject(workspaceId, branchName, infoPath, parentBranch)
      if (!result) return null

      registerPaneWorktree(result.claudePaneId, result.worktreeId)
      updateClaudeStatus(result.worktreeId, { status: 'ready' })

      // Pre-populate model from project settings
      try {
        const model = await invoke<string | null>('get_project_model', { projectPath: infoPath })
        if (model) updateClaudeStatus(result.worktreeId, { model })
      } catch { /* ignore */ }

      return result.worktreeId
    } catch (e) {
      console.error('Failed to add worktree:', e)
      throw e // Re-throw so UI can show error
    }
  }

  async function removeWorktree(
    workspaceId: string,
    worktreeId: string,
    mode: 'delete' | 'merge' | 'discard' | 'pr'
  ): Promise<void> {
    const ws = getPaneStore().getProjectWorkspace(workspaceId)
    if (!ws) return

    const wt = ws.worktrees.find(w => w.id === worktreeId)
    if (!wt) return

    // Don't allow removing the main worktree
    if (wt.isMain) {
      throw new Error('Cannot remove the main worktree')
    }

    try {
      if (mode === 'merge') {
        const mainWt = ws.worktrees.find(w => w.isMain)
        const targetBranch = mainWt?.branchName ?? 'main'
        await invoke('git_merge_branch', {
          repoRoot: ws.repoRoot,
          sourceBranch: wt.branchName,
          targetBranch,
        })
      }

      if (mode === 'pr') {
        await invoke('git_push_and_create_pr', { worktreePath: wt.path })
      }

      // Clean up watchers
      await unwatchAll(worktreeId)

      // Close PTY sessions FIRST so the OS releases file handles on the
      // worktree directory (otherwise `git worktree remove` fails on Windows
      // with "Permission denied" because the shell's CWD locks the folder).
      const collectLeaves = (node: any): string[] =>
        node.type === 'terminal' ? [node.id] : [...collectLeaves(node.first), ...collectLeaves(node.second)]
      const paneIdsToClose = collectLeaves(wt.root)
      for (const id of paneIdsToClose) {
        const sessionId = getPaneStore().getPtySession(id)
        if (sessionId) {
          try { await invoke('close_session', { sessionId }) } catch { /* ignore */ }
          getPaneStore().removePtySession(id)
        }
      }

      // Remove worktree from git (force if discarding)
      const force = mode === 'discard'
      await invoke('git_worktree_remove', { repoRoot: ws.repoRoot, worktreePath: wt.path, force })

      // Remove from store
      getPaneStore().removeWorktreeFromProject(workspaceId, worktreeId)

      // Clean up status and caches
      delete claudeStatuses.value[worktreeId]
      delete directoryCache.value[worktreeId]
      delete gitStatusCache.value[worktreeId]
      delete mergedWorktrees.value[worktreeId]
      delete paneToWorktree.value[wt.claudePaneId]
    } catch (e) {
      console.error('Failed to remove worktree:', e)
      throw e
    }
  }

  // Remove a worktree that is already merged. Skips the merge/PR steps and
  // doesn't depend on the worktree being clean — the branch tip lives on in
  // the parent branch, so a force remove is safe.
  async function removeMergedWorktree(workspaceId: string, worktreeId: string): Promise<void> {
    const ws = getPaneStore().getProjectWorkspace(workspaceId)
    if (!ws) return
    const wt = ws.worktrees.find(w => w.id === worktreeId)
    if (!wt || wt.isMain) return

    try {
      await unwatchAll(worktreeId)

      // Close PTY sessions before git remove (Windows file-lock workaround).
      const collectLeaves = (node: any): string[] =>
        node.type === 'terminal' ? [node.id] : [...collectLeaves(node.first), ...collectLeaves(node.second)]
      for (const id of collectLeaves(wt.root)) {
        const sessionId = getPaneStore().getPtySession(id)
        if (sessionId) {
          try { await invoke('close_session', { sessionId }) } catch { /* ignore */ }
          getPaneStore().removePtySession(id)
        }
      }

      await invoke('git_worktree_remove', { repoRoot: ws.repoRoot, worktreePath: wt.path, force: true })

      getPaneStore().removeWorktreeFromProject(workspaceId, worktreeId)

      delete claudeStatuses.value[worktreeId]
      delete directoryCache.value[worktreeId]
      delete gitStatusCache.value[worktreeId]
      delete mergedWorktrees.value[worktreeId]
      delete paneToWorktree.value[wt.claudePaneId]
    } catch (e) {
      console.error('Failed to remove merged worktree:', e)
      throw e
    }
  }

  function switchWorktree(workspaceId: string, worktreeId: string) {
    getPaneStore().switchWorktree(workspaceId, worktreeId)
  }

  // ── Init (called after restore) ───────────────────────────────────────────

  // Synchronously register every project worktree's claudePane → worktreeId
  // mapping. Must run before bootstrapBackgroundSessions so that event
  // handlers attached per-pane can resolve their worktree from the very
  // first claude-started event.
  function registerAllProjectPanes() {
    const paneStore = getPaneStore()
    for (const ws of paneStore.workspaces) {
      if (ws.type !== 'project') continue
      for (const wt of ws.worktrees) {
        registerPaneWorktree(wt.claudePaneId, wt.id)
        // Eagerly set card status when Claude is expected to run (seeded
        // during restore or workspace creation via the empty-string
        // sentinel in claudeSessionIds). This fires before any
        // TerminalPane mounts, so the sidebar card never flashes
        // "Terminal" when it should show "Idle".
        if (paneStore.isClaudeActive(wt.claudePaneId)) {
          updateClaudeStatus(wt.id, { status: 'ready' })
        }
      }
    }
  }

  async function initAllProjectWorkspaces() {
    const paneStore = getPaneStore()
    for (const ws of paneStore.workspaces) {
      if (ws.type !== 'project') continue
      for (const wt of ws.worktrees) {
        // Registration is idempotent; safe to call even if already
        // registered by registerAllProjectPanes.
        registerPaneWorktree(wt.claudePaneId, wt.id)
        // If Claude is expected to run (seeded by restore or workspace
        // creation), flip the card from "Terminal" → "Idle" immediately.
        // TerminalPane will later upgrade to "Working" via title events.
        if (paneStore.isClaudeActive(wt.claudePaneId)) {
          const cur = getClaudeStatus(wt.id)
          if (cur.status === 'exited') {
            updateClaudeStatus(wt.id, { status: 'ready' })
          }
        }
        try {
          const model = await invoke<string | null>('get_project_model', { projectPath: wt.path })
          if (model) updateClaudeStatus(wt.id, { model })
        } catch { /* ignore */ }
      }
      setupRefsWatcher(ws.id)
    }
  }

  // ── Cleanup ───────────────────────────────────────────────────────────────

  async function cleanupWorkspace(workspaceId: string) {
    const ws = getPaneStore().getProjectWorkspace(workspaceId)
    if (!ws) return

    await teardownRefsWatcher(workspaceId)

    for (const wt of ws.worktrees) {
      await unwatchAll(wt.id)
      delete claudeStatuses.value[wt.id]
      delete directoryCache.value[wt.id]
      delete gitStatusCache.value[wt.id]
      delete mergedWorktrees.value[wt.id]
      delete paneToWorktree.value[wt.claudePaneId]
    }
  }

  return {
    // Explorer
    directoryCache,
    gitStatusCache,
    loadDirectory,
    getCachedDirectory,
    refreshGitStatus,
    getFileStatus,
    getFolderStatus,
    watchDirectory,
    unwatchAll,
    // Claude status
    claudeStatuses,
    getClaudeStatus,
    updateClaudeStatus,
    paneToWorktree,
    registerPaneWorktree,
    getWorktreeIdForPane,
    // Worktree lifecycle
    createProjectWorkspace,
    addWorktree,
    removeWorktree,
    removeMergedWorktree,
    switchWorktree,
    registerAllProjectPanes,
    initAllProjectWorkspaces,
    // Merge tracking + actions
    mergedWorktrees,
    isMerged,
    checkMergedAll,
    manualMergeToParent,
    askClaudeToMerge,
    canAskClaudeToMerge,
    // Cleanup
    cleanupWorkspace,
  }
})
