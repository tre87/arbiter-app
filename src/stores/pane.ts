import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import type { PaneNode, TerminalLeaf, Workspace } from '../types/pane'
import type { SavedPaneNode, SavedTerminal, SavedWorkspace } from '../types/config'

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

export const usePaneStore = defineStore('pane', () => {
  // ── Multi-workspace state ─────────────────────────────────────────────────
  const initialLeaf: TerminalLeaf = { type: 'terminal', id: genId() }
  const workspaces = ref<Workspace[]>([{
    id: genId(),
    name: 'Workspace 1',
    root: initialLeaf,
    focusedId: initialLeaf.id,
  }])
  const activeWorkspaceIndex = ref(0)

  // Computed delegates to active workspace — all existing code reads/writes these
  const root = computed({
    get: () => workspaces.value[activeWorkspaceIndex.value].root,
    set: (val) => { workspaces.value[activeWorkspaceIndex.value].root = val },
  })
  const focusedId = computed({
    get: () => workspaces.value[activeWorkspaceIndex.value].focusedId,
    set: (val) => { workspaces.value[activeWorkspaceIndex.value].focusedId = val },
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
    return workspaces.value.some(ws => check(ws.root))
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

  // ── Active Claude session IDs (set by TerminalPane when Claude starts) ───
  const claudeSessionIds = ref<Record<string, string>>({})
  const claudeOutputTokens = ref<Record<string, number>>({})

  function setClaudeSessionId(paneId: string, sessionId: string, outputTokens?: number) {
    claudeSessionIds.value[paneId] = sessionId
    claudeOutputTokens.value[paneId] = outputTokens ?? 0
  }
  function clearClaudeSessionId(paneId: string) {
    delete claudeSessionIds.value[paneId]
    delete claudeOutputTokens.value[paneId]
  }
  function getClaudeSessionId(paneId: string): string | undefined {
    // Only return if there was actual conversation (output tokens > 0)
    if ((claudeOutputTokens.value[paneId] ?? 0) > 0) {
      return claudeSessionIds.value[paneId]
    }
    return undefined
  }
  function isClaudeRunning(paneId: string): boolean {
    return paneId in claudeSessionIds.value
  }

  // ── Saved metadata for restoration ────────────────────────────────────────
  // After restoreFromSaved, TerminalPane reads these to pass cwd / resume Claude
  const savedCwds = ref<Record<string, string>>({})
  const savedClaudeSessions = ref<Record<string, string>>({})
  const savedClaudeWasRunning = ref<Record<string, boolean>>({})

  function getSavedCwd(paneId: string): string | undefined {
    return savedCwds.value[paneId]
  }
  function consumeSavedCwd(paneId: string): string | undefined {
    const v = savedCwds.value[paneId]
    delete savedCwds.value[paneId]
    return v
  }
  function getSavedClaudeSession(paneId: string): string | undefined {
    return savedClaudeSessions.value[paneId]
  }
  function consumeSavedClaudeSession(paneId: string): string | undefined {
    const v = savedClaudeSessions.value[paneId]
    delete savedClaudeSessions.value[paneId]
    return v
  }
  function consumeSavedClaudeWasRunning(paneId: string): boolean {
    const v = savedClaudeWasRunning.value[paneId] ?? false
    delete savedClaudeWasRunning.value[paneId]
    return v
  }

  // ── Focus trigger ────────────────────────────────────────────────────────
  // Bumped by App.vue after layout restore so TerminalPane watchers can
  // pick up the initial focusedId (which was set before they mounted).
  const focusTrigger = ref(0)
  function triggerFocus() { focusTrigger.value++ }

  // ── Workspace CRUD ────────────────────────────────────────────────────────

  function addWorkspace() {
    const leaf: TerminalLeaf = { type: 'terminal', id: genId() }
    const ws: Workspace = {
      id: genId(),
      name: nextWorkspaceName(),
      root: leaf,
      focusedId: leaf.id,
    }
    workspaces.value.push(ws)
    activeWorkspaceIndex.value = workspaces.value.length - 1
    assignTerminalName(leaf.id)
  }

  function removeWorkspace(index: number) {
    if (workspaces.value.length <= 1) return

    // Collect all pane IDs in the workspace to clean up sessions
    const ws = workspaces.value[index]
    function collectPaneIds(node: PaneNode): string[] {
      if (node.type === 'terminal') return [node.id]
      return [...collectPaneIds(node.first), ...collectPaneIds(node.second)]
    }
    const paneIds = collectPaneIds(ws.root)

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

  function serializeWorkspace(ws: Workspace): { layout: SavedPaneNode; terminals: { id: string; name: string }[]; focusedTerminalIndex: number } {
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

  // Legacy single-workspace serialization (delegates to active workspace)
  function serializeLayout() {
    return serializeWorkspace(workspaces.value[activeWorkspaceIndex.value])
  }

  function serializeAll(): { workspaces: { name: string; layout: SavedPaneNode; terminals: { id: string; name: string }[]; focusedTerminalIndex: number }[]; activeWorkspaceIndex: number } {
    return {
      workspaces: workspaces.value.map(ws => ({
        name: ws.name,
        ...serializeWorkspace(ws),
      })),
      activeWorkspaceIndex: activeWorkspaceIndex.value,
    }
  }

  function buildWorkspace(saved: SavedPaneNode, terminals: SavedTerminal[], focusedTerminalIndex?: number, wsName?: string): Workspace {
    const terminalIdsByIndex: string[] = []

    function build(node: SavedPaneNode): PaneNode {
      if (node.type === 'terminal') {
        const id = genId()
        const t = terminals[node.index]
        terminalIdsByIndex[node.index] = id
        if (t) {
          terminalNames.value[id] = t.name
          if (t.cwd) savedCwds.value[id] = t.cwd
          if (t.claudeSessionId) savedClaudeSessions.value[id] = t.claudeSessionId
          if (t.claudeWasRunning) savedClaudeWasRunning.value[id] = true
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
      id: genId(),
      name: wsName ?? nextWorkspaceName(),
      root: newRoot,
      focusedId: restoredFocusId ?? firstLeaf(newRoot),
    }
  }

  // Legacy single-workspace restore
  function restoreFromSaved(saved: SavedPaneNode, terminals: SavedTerminal[], focusedTerminalIndex?: number) {
    nextId = 1
    terminalNames.value = {}
    ptySessionIds.value = {}
    savedCwds.value = {}
    savedClaudeSessions.value = {}
    savedClaudeWasRunning.value = {}

    const ws = buildWorkspace(saved, terminals, focusedTerminalIndex, 'Workspace 1')
    workspaces.value = [ws]
    activeWorkspaceIndex.value = 0
  }

  function restoreAllWorkspaces(savedWorkspaces: SavedWorkspace[], savedActiveIndex?: number) {
    nextId = 1
    terminalNames.value = {}
    ptySessionIds.value = {}
    savedCwds.value = {}
    savedClaudeSessions.value = {}
    savedClaudeWasRunning.value = {}

    const restored = savedWorkspaces.map(sw =>
      buildWorkspace(sw.layout, sw.terminals, sw.focusedTerminalIndex, sw.name)
    )

    if (restored.length === 0) {
      const leaf: TerminalLeaf = { type: 'terminal', id: genId() }
      assignTerminalName(leaf.id)
      restored.push({
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
    // Pane operations
    splitFocused, closeFocused, setFocus,
    updateSplitSizes, adjustSplitSize,
    // PTY session mapping
    setPtySession, getPtySession, hasPaneId, removePtySession,
    // Terminal names
    terminalNames, getTerminalName, setTerminalName, assignTerminalName,
    // Claude session tracking
    claudeSessionIds, setClaudeSessionId, clearClaudeSessionId, getClaudeSessionId, isClaudeRunning,
    // Saved state for restoration
    savedCwds, savedClaudeSessions,
    getSavedCwd, consumeSavedCwd, getSavedClaudeSession, consumeSavedClaudeSession, consumeSavedClaudeWasRunning,
    // Focus trigger
    focusTrigger, triggerFocus,
    // Serialization
    serializeLayout, serializeAll,
    restoreFromSaved, restoreAllWorkspaces,
  }
})
