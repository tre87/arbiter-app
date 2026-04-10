import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import { emit } from '@tauri-apps/api/event'
import type { PaneNode, TerminalLeaf, SplitNode, Workspace, TerminalWorkspace, ProjectWorkspace, Worktree, ClaudePaneState } from '../types/pane'
import type { SavedPaneNode, SavedTerminal, SavedWorkspace, SavedTerminalWorkspace, SavedProjectWorkspace, LegacySavedWorkspace } from '../types/config'

let nextId = 1
const genId = () => String(nextId++)

function nextAvailableNumber(prefix: string, usedNames: Iterable<string>): number {
  const used = new Set<number>()
  for (const name of usedNames) {
    const match = name.match(new RegExp(`^${prefix} (\\d+)$`))
    if (match) used.add(parseInt(match[1], 10))
  }
  let n = 1
  while (used.has(n)) n++
  return n
}

// ── Helpers for workspace-type-aware tree access ────────────────────────────

function getWorkspaceRoot(ws: Workspace): PaneNode {
  if (ws.type === 'project') {
    const wt = ws.worktrees.find(w => w.id === ws.activeWorktreeId)
    return wt ? wt.root : ws.worktrees[0].root
  }
  return ws.root
}

function setWorkspaceRoot(ws: Workspace, val: PaneNode) {
  if (ws.type === 'project') {
    const wt = ws.worktrees.find(w => w.id === ws.activeWorktreeId)
    if (wt) wt.root = val
  } else {
    ws.root = val
  }
}

function getWorkspaceFocusedId(ws: Workspace): string {
  return ws.type === 'project' ? ws.focusedPaneId : ws.focusedId
}

function setWorkspaceFocusedId(ws: Workspace, val: string) {
  if (ws.type === 'project') {
    ws.focusedPaneId = val
  } else {
    ws.focusedId = val
  }
}

function collectPaneIdsFromNode(node: PaneNode): string[] {
  if (node.type === 'terminal') return [node.id]
  return [...collectPaneIdsFromNode(node.first), ...collectPaneIdsFromNode(node.second)]
}

function collectAllPaneIds(ws: Workspace): string[] {
  if (ws.type === 'project') {
    return ws.worktrees.flatMap(wt => collectPaneIdsFromNode(wt.root))
  }
  return collectPaneIdsFromNode(ws.root)
}

export const usePaneStore = defineStore('pane', () => {
  // ── Multi-workspace state ─────────────────────────────────────────────────
  const initialLeaf: TerminalLeaf = { type: 'terminal', id: genId() }
  const workspaces = ref<Workspace[]>([{
    type: 'terminal',
    id: genId(),
    name: 'Workspace 1',
    root: initialLeaf,
    focusedId: initialLeaf.id,
  }])
  const activeWorkspaceIndex = ref(0)

  // Computed delegates to active workspace — all existing code reads/writes these
  const root = computed({
    get: () => getWorkspaceRoot(workspaces.value[activeWorkspaceIndex.value]),
    set: (val) => setWorkspaceRoot(workspaces.value[activeWorkspaceIndex.value], val),
  })
  const focusedId = computed({
    get: () => getWorkspaceFocusedId(workspaces.value[activeWorkspaceIndex.value]),
    set: (val) => setWorkspaceFocusedId(workspaces.value[activeWorkspaceIndex.value], val),
  })

  // Maps paneId → PTY sessionId so sessions survive Vue remounts (e.g. splits)
  const ptySessionIds = ref<Record<string, string>>({})

  // Terminal display names
  const terminalNames = ref<Record<string, string>>({})

  function assignTerminalName(id: string) {
    const n = nextAvailableNumber('Terminal', Object.values(terminalNames.value))
    terminalNames.value[id] = `Terminal ${n}`
  }

  function getTerminalName(id: string): string {
    return terminalNames.value[id] ?? 'Terminal'
  }

  function setTerminalName(id: string, name: string) {
    terminalNames.value[id] = name
  }

  // Terminal shell types (for persistence across restarts)
  const terminalShells = ref<Record<string, 'powershell' | 'gitbash'>>({})

  function setTerminalShell(id: string, shell: 'powershell' | 'gitbash') {
    terminalShells.value[id] = shell
  }

  function getTerminalShell(id: string): 'powershell' | 'gitbash' {
    return terminalShells.value[id] ?? 'powershell'
  }

  function nextWorkspaceName(): string {
    const n = nextAvailableNumber('Workspace', workspaces.value.map(ws => ws.name))
    return `Workspace ${n}`
  }

  // Assign name to the initial leaf
  assignTerminalName(initialLeaf.id)

  function setPtySession(paneId: string, sessionId: string) {
    ptySessionIds.value[paneId] = sessionId
  }

  function getPtySession(paneId: string): string | undefined {
    return ptySessionIds.value[paneId]
  }

  function hasPaneId(id: string): boolean {
    function check(node: PaneNode): boolean {
      if (node.type === 'terminal') return node.id === id
      return check(node.first) || check(node.second)
    }
    // Check all workspaces — pane may be in a background tab
    return workspaces.value.some(ws => {
      if (ws.type === 'project') {
        return ws.worktrees.some(wt => check(wt.root))
      }
      return check(ws.root)
    })
  }

  // True if the given pane lives inside any project workspace's worktree tree.
  // Used by TerminalPane to hide its footer (git/cwd info is shown in the
  // worktree sidebar instead).
  function isPaneInProjectWorkspace(id: string): boolean {
    function check(node: PaneNode): boolean {
      if (node.type === 'terminal') return node.id === id
      return check(node.first) || check(node.second)
    }
    return workspaces.value.some(ws => {
      if (ws.type !== 'project') return false
      return ws.worktrees.some(wt => check(wt.root))
    })
  }

  function removePtySession(paneId: string) {
    delete ptySessionIds.value[paneId]
  }

  function splitFocused(direction: 'vertical' | 'horizontal') {
    const newLeaf: TerminalLeaf = { type: 'terminal', id: genId() }
    const newSplitId = genId()

    function replace(node: PaneNode): PaneNode {
      if (node.type === 'terminal' && node.id === focusedId.value) {
        return {
          type: 'split',
          id: newSplitId,
          direction,
          sizes: [50, 50],
          first: node,
          second: newLeaf,
        }
      }
      if (node.type === 'split') {
        return { ...node, first: replace(node.first), second: replace(node.second) }
      }
      return node
    }

    root.value = replace(root.value)
    focusedId.value = newLeaf.id
    assignTerminalName(newLeaf.id)
  }

  function closeFocused() {
    const ws = workspaces.value[activeWorkspaceIndex.value]

    // For project workspaces, prevent closing the Claude pane
    if (ws.type === 'project') {
      const activeWt = ws.worktrees.find(w => w.id === ws.activeWorktreeId)
      if (activeWt && focusedId.value === activeWt.claudePaneId) return
    }

    // Can't close the last pane
    if (root.value.type === 'terminal') return

    const target = focusedId.value
    let sibling: PaneNode | null = null
    let targetWasSecond = false

    // Replace the parent split of the target with its sibling
    function remove(node: PaneNode): PaneNode {
      if (node.type !== 'split') return node
      if (node.first.type === 'terminal' && node.first.id === target) {
        sibling = node.second
        targetWasSecond = false
        return node.second
      }
      if (node.second.type === 'terminal' && node.second.id === target) {
        sibling = node.first
        targetWasSecond = true
        return node.first
      }
      return { ...node, first: remove(node.first), second: remove(node.second) }
    }

    root.value = remove(root.value)
    delete terminalStatuses.value[target]

    // Focus the leaf in the sibling closest to where the closed pane was
    function firstLeaf(node: PaneNode): string {
      if (node.type === 'terminal') return node.id
      return firstLeaf(node.first)
    }
    function lastLeaf(node: PaneNode): string {
      if (node.type === 'terminal') return node.id
      return lastLeaf(node.second)
    }

    if (sibling) {
      focusedId.value = targetWasSecond ? lastLeaf(sibling) : firstLeaf(sibling)
    }
  }

  function setFocus(id: string) {
    focusedId.value = id
  }

  function adjustSplitSize(splitId: string, delta: number) {
    function update(node: PaneNode): PaneNode {
      if (node.type === 'split' && node.id === splitId) {
        const newFirst = Math.max(5, Math.min(95, node.sizes[0] + delta))
        return { ...node, sizes: [newFirst, 100 - newFirst] }
      }
      if (node.type === 'split') {
        return { ...node, first: update(node.first), second: update(node.second) }
      }
      return node
    }
    root.value = update(root.value)
  }

  function updateSplitSizes(splitId: string, sizes: [number, number]) {
    function update(node: PaneNode): PaneNode {
      if (node.type === 'split' && node.id === splitId) {
        return { ...node, sizes }
      }
      if (node.type === 'split') {
        return { ...node, first: update(node.first), second: update(node.second) }
      }
      return node
    }
    root.value = update(root.value)
  }

  // ── Terminal status tracking (for workspace overview) ─────────────────────
  const terminalStatuses = ref<Record<string, 'idle' | 'running' | 'ready' | 'working' | 'attention'>>({})

  function setTerminalStatus(paneId: string, status: 'idle' | 'running' | 'ready' | 'working' | 'attention') {
    terminalStatuses.value[paneId] = status
    emitOverviewUpdate()
  }

  function emitOverviewUpdate() {
    const terminals = getAllTerminals().map(t => ({
      paneId: t.paneId,
      workspaceIndex: t.workspaceIndex,
      workspaceName: t.workspaceName,
      workspaceType: t.workspaceType,
      name: getTerminalName(t.paneId),
      status: getTerminalStatus(t.paneId),
    }))
    emit('overview-update', terminals)
  }

  function getTerminalStatus(paneId: string): 'idle' | 'running' | 'ready' | 'working' | 'attention' {
    return terminalStatuses.value[paneId] ?? 'idle'
  }

  function getAllTerminals(): Array<{ paneId: string; workspaceIndex: number; workspaceName: string; workspaceType: 'terminal' | 'project' }> {
    const result: Array<{ paneId: string; workspaceIndex: number; workspaceName: string; workspaceType: 'terminal' | 'project' }> = []
    for (let i = 0; i < workspaces.value.length; i++) {
      const ws = workspaces.value[i]
      function collect(node: PaneNode) {
        if (node.type === 'terminal') {
          result.push({ paneId: node.id, workspaceIndex: i, workspaceName: ws.name, workspaceType: ws.type })
        } else {
          collect(node.first)
          collect(node.second)
        }
      }
      if (ws.type === 'project') {
        for (const wt of ws.worktrees) {
          collect(wt.root)
        }
      } else {
        collect(ws.root)
      }
    }
    return result
  }

  // ── Centralized Claude lifecycle state (per pane) ─────────────────────────
  const claudePaneStates = ref<Record<string, ClaudePaneState>>({})

  const defaultClaudePaneState: ClaudePaneState = {
    lifecycle: 'closed', sessionId: null, confirmed: false,
    model: null, inputTokens: 0, outputTokens: 0,
    cacheReadTokens: 0, cacheWriteTokens: 0, contextPercent: 0,
  }

  function getClaudePaneState(paneId: string): ClaudePaneState {
    return claudePaneStates.value[paneId] ?? { ...defaultClaudePaneState }
  }

  function updateClaudePaneState(paneId: string, update: Partial<ClaudePaneState>) {
    const current = getClaudePaneState(paneId)
    claudePaneStates.value[paneId] = { ...current, ...update }
  }

  function clearClaudePaneState(paneId: string) {
    delete claudePaneStates.value[paneId]
  }

  function isClaudeActive(paneId: string): boolean {
    const s = claudePaneStates.value[paneId]
    return !!s && s.lifecycle !== 'closed'
  }

  function getClaudeSessionForSave(paneId: string): { sessionId: string | null; wasOpen: boolean } {
    const s = claudePaneStates.value[paneId]
    if (!s || s.lifecycle === 'closed') return { sessionId: null, wasOpen: false }
    return {
      sessionId: s.confirmed && s.sessionId ? s.sessionId : null,
      wasOpen: true,
    }
  }

  // ── Saved metadata for restoration ────────────────────────────────────────
  // After restoreFromSaved, TerminalPane reads these to pass cwd / resume Claude
  const savedCwds = ref<Record<string, string>>({})
  const savedClaudeRestore = ref<Record<string, { sessionId: string | null; wasOpen: boolean }>>({})
  const savedShells = ref<Record<string, 'powershell' | 'gitbash'>>({})

  function getSavedCwd(paneId: string): string | undefined {
    return savedCwds.value[paneId]
  }
  function consumeSavedCwd(paneId: string): string | undefined {
    const v = savedCwds.value[paneId]
    delete savedCwds.value[paneId]
    return v
  }
  function consumeSavedClaudeRestore(paneId: string): { sessionId: string | null; wasOpen: boolean } | undefined {
    const v = savedClaudeRestore.value[paneId]
    delete savedClaudeRestore.value[paneId]
    return v
  }
  function consumeSavedShell(paneId: string): 'powershell' | 'gitbash' | undefined {
    const v = savedShells.value[paneId]
    delete savedShells.value[paneId]
    return v
  }
  function getSavedShell(paneId: string): 'powershell' | 'gitbash' | undefined {
    return savedShells.value[paneId]
  }

  // ── Focus trigger ────────────────────────────────────────────────────────
  // Bumped by App.vue after layout restore so TerminalPane watchers can
  // pick up the initial focusedId (which was set before they mounted).
  const focusTrigger = ref(0)
  function triggerFocus() { focusTrigger.value++ }

  // ── Workspace CRUD ────────────────────────────────────────────────────────

  function addWorkspace() {
    const leaf: TerminalLeaf = { type: 'terminal', id: genId() }
    const ws: TerminalWorkspace = {
      type: 'terminal',
      id: genId(),
      name: nextWorkspaceName(),
      root: leaf,
      focusedId: leaf.id,
    }
    workspaces.value.push(ws)
    activeWorkspaceIndex.value = workspaces.value.length - 1
    assignTerminalName(leaf.id)
  }

  function addProjectWorkspace(name: string, repoRoot: string, mainBranchName: string, mainWorktreePath: string) {
    const claudeLeaf: TerminalLeaf = { type: 'terminal', id: genId() }
    const termLeaf: TerminalLeaf = { type: 'terminal', id: genId() }

    const worktreeRoot: SplitNode = {
      type: 'split',
      id: genId(),
      direction: 'horizontal',
      sizes: [80, 20],
      first: claudeLeaf,
      second: termLeaf,
    }

    const worktreeId = genId()
    const worktree: Worktree = {
      id: worktreeId,
      branchName: mainBranchName,
      path: mainWorktreePath,
      isMain: true,
      parentBranch: null,
      claudePaneId: claudeLeaf.id,
      defaultTerminalPaneId: termLeaf.id,
      root: worktreeRoot,
      explorerExpandedPaths: [],
    }

    const ws: ProjectWorkspace = {
      type: 'project',
      id: genId(),
      name,
      repoRoot,
      worktrees: [worktree],
      activeWorktreeId: worktreeId,
      focusedPaneId: claudeLeaf.id,
    }

    // Seed saved state BEFORE pushing the workspace — once the ref is pushed,
    // Vue can synchronously mount TerminalPane in the next render pass, and
    // its mount hook immediately `consumeSavedCwd`s this pane id.
    terminalNames.value[claudeLeaf.id] = 'Claude'
    terminalNames.value[termLeaf.id] = 'Terminal'
    savedCwds.value[claudeLeaf.id] = mainWorktreePath
    savedCwds.value[termLeaf.id] = mainWorktreePath
    // Auto-launch claude in the main worktree's Claude pane on first mount.
    savedClaudeRestore.value[claudeLeaf.id] = { sessionId: null, wasOpen: true }
    claudePaneStates.value[claudeLeaf.id] = { ...defaultClaudePaneState, lifecycle: 'launching' }

    workspaces.value.push(ws)
    activeWorkspaceIndex.value = workspaces.value.length - 1

    return { workspaceId: ws.id, worktreeId, claudePaneId: claudeLeaf.id, defaultTerminalPaneId: termLeaf.id }
  }

  function addWorktreeToProject(workspaceId: string, branchName: string, worktreePath: string, parentBranch: string | null) {
    const ws = workspaces.value.find(w => w.id === workspaceId)
    if (!ws || ws.type !== 'project') return null

    const claudeLeaf: TerminalLeaf = { type: 'terminal', id: genId() }
    const termLeaf: TerminalLeaf = { type: 'terminal', id: genId() }

    const worktreeRoot: SplitNode = {
      type: 'split',
      id: genId(),
      direction: 'horizontal',
      sizes: [80, 20],
      first: claudeLeaf,
      second: termLeaf,
    }

    const worktreeId = genId()
    const worktree: Worktree = {
      id: worktreeId,
      branchName,
      path: worktreePath,
      isMain: false,
      parentBranch,
      claudePaneId: claudeLeaf.id,
      defaultTerminalPaneId: termLeaf.id,
      root: worktreeRoot,
      explorerExpandedPaths: [],
    }

    // Seed saved state BEFORE the worktree push so TerminalPane's mount
    // hook sees the cwd/claude-resume data on first render.
    terminalNames.value[claudeLeaf.id] = 'Claude'
    terminalNames.value[termLeaf.id] = 'Terminal'
    savedCwds.value[claudeLeaf.id] = worktreePath
    savedCwds.value[termLeaf.id] = worktreePath
    // Auto-launch claude in the worktree's main (Claude) pane on first mount.
    // Also pre-mark it as "claude running" so the autosave persists the intent
    // even if the app exits before claude actually starts (~500ms after PTY).
    savedClaudeRestore.value[claudeLeaf.id] = { sessionId: null, wasOpen: true }
    claudePaneStates.value[claudeLeaf.id] = { ...defaultClaudePaneState, lifecycle: 'launching' }

    ws.worktrees.push(worktree)
    ws.activeWorktreeId = worktreeId
    ws.focusedPaneId = claudeLeaf.id

    return { worktreeId, claudePaneId: claudeLeaf.id, defaultTerminalPaneId: termLeaf.id }
  }

  function removeWorktreeFromProject(workspaceId: string, worktreeId: string): string[] {
    const ws = workspaces.value.find(w => w.id === workspaceId)
    if (!ws || ws.type !== 'project') return []

    const idx = ws.worktrees.findIndex(wt => wt.id === worktreeId)
    if (idx < 0) return []

    const wt = ws.worktrees[idx]
    const paneIds = collectPaneIdsFromNode(wt.root)

    ws.worktrees.splice(idx, 1)

    // If we removed the active worktree, switch to another
    if (ws.activeWorktreeId === worktreeId && ws.worktrees.length > 0) {
      ws.activeWorktreeId = ws.worktrees[0].id
      ws.focusedPaneId = ws.worktrees[0].claudePaneId
    }

    // Clean up statuses
    for (const id of paneIds) delete terminalStatuses.value[id]

    return paneIds
  }

  function switchWorktree(workspaceId: string, worktreeId: string) {
    const ws = workspaces.value.find(w => w.id === workspaceId)
    if (!ws || ws.type !== 'project') return
    const wt = ws.worktrees.find(w => w.id === worktreeId)
    if (!wt) return
    ws.activeWorktreeId = worktreeId
    ws.focusedPaneId = wt.claudePaneId
  }

  function getActiveWorktree(workspaceId: string): Worktree | undefined {
    const ws = workspaces.value.find(w => w.id === workspaceId)
    if (!ws || ws.type !== 'project') return undefined
    return ws.worktrees.find(w => w.id === ws.activeWorktreeId)
  }

  function getProjectWorkspace(workspaceId: string): ProjectWorkspace | undefined {
    const ws = workspaces.value.find(w => w.id === workspaceId)
    return ws?.type === 'project' ? ws : undefined
  }


  function removeWorkspace(index: number) {
    if (workspaces.value.length <= 1) return

    const ws = workspaces.value[index]
    const paneIds = collectAllPaneIds(ws)
    for (const id of paneIds) delete terminalStatuses.value[id]

    workspaces.value.splice(index, 1)

    // Adjust active index
    if (activeWorkspaceIndex.value >= workspaces.value.length) {
      activeWorkspaceIndex.value = workspaces.value.length - 1
    } else if (activeWorkspaceIndex.value > index) {
      activeWorkspaceIndex.value--
    }

    return paneIds // Caller can use these to close PTY sessions
  }

  function switchWorkspace(index: number) {
    if (index >= 0 && index < workspaces.value.length) {
      activeWorkspaceIndex.value = index
    }
  }

  function moveWorkspace(from: number, to: number) {
    if (from === to) return
    const active = activeWorkspaceIndex.value
    const ws = workspaces.value.splice(from, 1)[0]
    workspaces.value.splice(to, 0, ws)
    // Adjust active index to follow the previously active workspace
    if (active === from) {
      activeWorkspaceIndex.value = to
    } else if (from < active && to >= active) {
      activeWorkspaceIndex.value--
    } else if (from > active && to <= active) {
      activeWorkspaceIndex.value++
    }
  }

  function renameWorkspace(index: number, name: string) {
    if (index >= 0 && index < workspaces.value.length) {
      workspaces.value[index].name = name
    }
  }

  // ── Serialization ────────────────────────────────────────────────────────

  function serializeTerminalWorkspace(ws: TerminalWorkspace): { layout: SavedPaneNode; terminals: { id: string; name: string }[]; focusedTerminalIndex: number } {
    const terminals: { id: string; name: string }[] = []

    function walk(node: PaneNode): SavedPaneNode {
      if (node.type === 'terminal') {
        const idx = terminals.length
        terminals.push({ id: node.id, name: getTerminalName(node.id) })
        return { type: 'terminal', index: idx }
      }
      return {
        type: 'split',
        direction: node.direction,
        sizes: [...node.sizes] as [number, number],
        first: walk(node.first),
        second: walk(node.second),
      }
    }

    const layout = walk(ws.root)
    const focusedTerminalIndex = terminals.findIndex(t => t.id === ws.focusedId)
    return { layout, terminals, focusedTerminalIndex: focusedTerminalIndex >= 0 ? focusedTerminalIndex : 0 }
  }

  function serializeWorktree(wt: Worktree): { layout: SavedPaneNode; terminals: { id: string; name: string }[]; claudePaneIndex: number; defaultTerminalIndex: number } {
    const terminals: { id: string; name: string }[] = []

    function walk(node: PaneNode): SavedPaneNode {
      if (node.type === 'terminal') {
        const idx = terminals.length
        terminals.push({ id: node.id, name: getTerminalName(node.id) })
        return { type: 'terminal', index: idx }
      }
      return {
        type: 'split',
        direction: node.direction,
        sizes: [...node.sizes] as [number, number],
        first: walk(node.first),
        second: walk(node.second),
      }
    }

    const layout = walk(wt.root)
    const claudePaneIndex = terminals.findIndex(t => t.id === wt.claudePaneId)
    const defaultTerminalIndex = terminals.findIndex(t => t.id === wt.defaultTerminalPaneId)
    return { layout, terminals, claudePaneIndex, defaultTerminalIndex }
  }

  // Legacy single-workspace serialization (delegates to active workspace)
  function serializeLayout() {
    const ws = workspaces.value[activeWorkspaceIndex.value]
    if (ws.type === 'project') {
      // Fallback: serialize the active worktree's root as if it were a terminal workspace
      const wt = ws.worktrees.find(w => w.id === ws.activeWorktreeId)!
      const terminals: { id: string; name: string }[] = []
      function walk(node: PaneNode): SavedPaneNode {
        if (node.type === 'terminal') {
          const idx = terminals.length
          terminals.push({ id: node.id, name: getTerminalName(node.id) })
          return { type: 'terminal', index: idx }
        }
        return { type: 'split', direction: node.direction, sizes: [...node.sizes] as [number, number], first: walk(node.first), second: walk(node.second) }
      }
      const layout = walk(wt.root)
      return { layout, terminals, focusedTerminalIndex: 0 }
    }
    return serializeTerminalWorkspace(ws)
  }

  // Enrichable format — includes live pane IDs so App.vue can fill in CWD/Claude/shell
  interface EnrichableTerminal { id: string; name: string }
  interface EnrichableWorktree {
    branchName: string; path: string; isMain: boolean
    parentBranch: string | null
    claudePaneIndex: number; defaultTerminalIndex: number
    layout: SavedPaneNode; terminals: EnrichableTerminal[]
    explorerExpandedPaths: string[]
  }
  type EnrichableWorkspace =
    | { type: 'terminal'; name: string; layout: SavedPaneNode; terminals: EnrichableTerminal[]; focusedTerminalIndex: number }
    | { type: 'project'; name: string; repoRoot: string; worktrees: EnrichableWorktree[]; activeWorktreeId: string }

  function serializeAll(): { workspaces: EnrichableWorkspace[]; activeWorkspaceIndex: number } {
    const result: EnrichableWorkspace[] = workspaces.value.map(ws => {
      if (ws.type === 'project') {
        return {
          type: 'project' as const,
          name: ws.name,
          repoRoot: ws.repoRoot,
          worktrees: ws.worktrees.map(wt => {
            const s = serializeWorktree(wt)
            return {
              branchName: wt.branchName,
              path: wt.path,
              isMain: wt.isMain,
              parentBranch: wt.parentBranch,
              claudePaneIndex: s.claudePaneIndex,
              defaultTerminalIndex: s.defaultTerminalIndex,
              layout: s.layout,
              terminals: s.terminals,
              explorerExpandedPaths: [...wt.explorerExpandedPaths],
            }
          }),
          activeWorktreeId: ws.activeWorktreeId,
        }
      }

      const s = serializeTerminalWorkspace(ws)
      return {
        type: 'terminal' as const,
        name: ws.name,
        layout: s.layout,
        terminals: s.terminals,
        focusedTerminalIndex: s.focusedTerminalIndex,
      }
    })

    return { workspaces: result, activeWorkspaceIndex: activeWorkspaceIndex.value }
  }

  function buildTerminalWorkspace(saved: SavedPaneNode, terminals: SavedTerminal[], focusedTerminalIndex?: number, wsName?: string): TerminalWorkspace {
    const terminalIdsByIndex: string[] = []

    function build(node: SavedPaneNode): PaneNode {
      if (node.type === 'terminal') {
        const id = genId()
        const t = terminals[node.index]
        terminalIdsByIndex[node.index] = id
        if (t) {
          terminalNames.value[id] = t.name
          if (t.cwd) savedCwds.value[id] = t.cwd
          if (t.claudeSessionId || t.claudeWasRunning) {
            savedClaudeRestore.value[id] = {
              sessionId: t.claudeSessionId ?? null,
              wasOpen: t.claudeWasRunning ?? !!t.claudeSessionId,
            }
            // Pre-seed lifecycle so isClaudeActive returns true immediately.
            // Without this, the reactive watcher in project.ts would see
            // 'closed' and set the card to 'exited' before TerminalPane mounts.
            claudePaneStates.value[id] = { ...defaultClaudePaneState, lifecycle: 'launching' }
          }
          if (t.shell) savedShells.value[id] = t.shell
        } else {
          assignTerminalName(id)
        }
        return { type: 'terminal', id }
      }
      const id = genId()
      return {
        type: 'split',
        id,
        direction: node.direction,
        sizes: [...node.sizes] as [number, number],
        first: build(node.first),
        second: build(node.second),
      }
    }

    const newRoot = build(saved)
    const restoredFocusId = focusedTerminalIndex != null ? terminalIdsByIndex[focusedTerminalIndex] : undefined

    function firstLeaf(node: PaneNode): string {
      if (node.type === 'terminal') return node.id
      return firstLeaf(node.first)
    }

    return {
      type: 'terminal',
      id: genId(),
      name: wsName ?? nextWorkspaceName(),
      root: newRoot,
      focusedId: restoredFocusId ?? firstLeaf(newRoot),
    }
  }

  function buildProjectWorkspace(saved: SavedProjectWorkspace): ProjectWorkspace {
    const worktrees: Worktree[] = saved.worktrees.map(sw => {
      const terminalIdsByIndex: string[] = []

      function build(node: SavedPaneNode): PaneNode {
        if (node.type === 'terminal') {
          const id = genId()
          const t = sw.terminals[node.index]
          terminalIdsByIndex[node.index] = id
          if (t) {
            terminalNames.value[id] = t.name
            if (t.cwd) savedCwds.value[id] = t.cwd
            if (t.claudeSessionId || t.claudeWasRunning) {
              savedClaudeRestore.value[id] = {
                sessionId: t.claudeSessionId ?? null,
                wasOpen: t.claudeWasRunning ?? !!t.claudeSessionId,
              }
              claudePaneStates.value[id] = { ...defaultClaudePaneState, lifecycle: 'launching' }
            }
            if (t.shell) savedShells.value[id] = t.shell
          } else {
            assignTerminalName(id)
          }
          return { type: 'terminal', id }
        }
        const id = genId()
        return {
          type: 'split',
          id,
          direction: node.direction,
          sizes: [...node.sizes] as [number, number],
          first: build(node.first),
          second: build(node.second),
        }
      }

      const wtRoot = build(sw.layout)
      const worktreeId = genId()

      return {
        id: worktreeId,
        branchName: sw.branchName,
        path: sw.path,
        isMain: sw.isMain,
        parentBranch: sw.parentBranch ?? null,
        claudePaneId: terminalIdsByIndex[sw.claudePaneIndex] ?? '',
        defaultTerminalPaneId: terminalIdsByIndex[sw.defaultTerminalIndex] ?? '',
        root: wtRoot,
        explorerExpandedPaths: sw.explorerExpandedPaths ?? [],
      }
    })

    const activeWt = worktrees[0]
    return {
      type: 'project',
      id: genId(),
      name: saved.name,
      repoRoot: saved.repoRoot,
      worktrees,
      activeWorktreeId: activeWt?.id ?? '',
      focusedPaneId: activeWt?.claudePaneId ?? '',
    }
  }

  // Legacy single-workspace restore
  function restoreFromSaved(saved: SavedPaneNode, terminals: SavedTerminal[], focusedTerminalIndex?: number) {
    nextId = 1
    terminalNames.value = {}
    ptySessionIds.value = {}
    savedCwds.value = {}
    savedClaudeRestore.value = {}
    claudePaneStates.value = {}
    savedShells.value = {}

    const ws = buildTerminalWorkspace(saved, terminals, focusedTerminalIndex, 'Workspace 1')
    workspaces.value = [ws]
    activeWorkspaceIndex.value = 0
  }

  function restoreAllWorkspaces(savedWorkspaces: (SavedWorkspace | LegacySavedWorkspace)[], savedActiveIndex?: number) {
    nextId = 1
    terminalNames.value = {}
    ptySessionIds.value = {}
    savedCwds.value = {}
    savedClaudeRestore.value = {}
    claudePaneStates.value = {}
    savedShells.value = {}

    const restored: Workspace[] = savedWorkspaces.map(sw => {
      // Handle typed workspaces
      if ('type' in sw && sw.type === 'project') {
        return buildProjectWorkspace(sw as SavedProjectWorkspace)
      }
      // Terminal workspace (new format with type, or legacy without type)
      const tsw = sw as SavedTerminalWorkspace | LegacySavedWorkspace
      return buildTerminalWorkspace(tsw.layout, tsw.terminals, tsw.focusedTerminalIndex, tsw.name)
    })

    if (restored.length === 0) {
      const leaf: TerminalLeaf = { type: 'terminal', id: genId() }
      assignTerminalName(leaf.id)
      restored.push({
        type: 'terminal',
        id: genId(),
        name: nextWorkspaceName(),
        root: leaf,
        focusedId: leaf.id,
      })
    }

    workspaces.value = restored
    activeWorkspaceIndex.value = (savedActiveIndex != null && savedActiveIndex < restored.length) ? savedActiveIndex : 0
  }

  return {
    // Workspace state
    workspaces, activeWorkspaceIndex,
    root, focusedId,
    // Workspace CRUD
    addWorkspace, removeWorkspace, switchWorkspace, moveWorkspace, renameWorkspace,
    // Project workspace operations
    addProjectWorkspace, addWorktreeToProject, removeWorktreeFromProject,
    switchWorktree, getActiveWorktree, getProjectWorkspace,
    // Pane operations
    splitFocused, closeFocused, setFocus,
    updateSplitSizes, adjustSplitSize,
    // PTY session mapping
    setPtySession, getPtySession, hasPaneId, isPaneInProjectWorkspace, removePtySession,
    // Terminal names
    terminalNames, getTerminalName, setTerminalName, assignTerminalName,
    terminalShells, getTerminalShell, setTerminalShell,
    // Claude lifecycle state (centralized)
    claudePaneStates, getClaudePaneState, updateClaudePaneState, clearClaudePaneState,
    isClaudeActive, getClaudeSessionForSave,
    // Terminal status
    terminalStatuses, setTerminalStatus, getTerminalStatus, getAllTerminals, emitOverviewUpdate,
    // Saved state for restoration
    savedCwds, savedClaudeRestore,
    getSavedCwd, consumeSavedCwd, consumeSavedClaudeRestore, getSavedShell, consumeSavedShell,
    // Focus trigger
    focusTrigger, triggerFocus,
    // Serialization
    serializeLayout, serializeAll,
    restoreFromSaved, restoreAllWorkspaces,
  }
})
