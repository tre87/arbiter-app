import { defineStore } from 'pinia'
import { ref } from 'vue'
import type { PaneNode, TerminalLeaf } from '../types/pane'
import type { SavedPaneNode, SavedTerminal } from '../types/config'

let nextId = 1
const genId = () => String(nextId++)

let nextTerminalNumber = 1

export const usePaneStore = defineStore('pane', () => {
  const initialLeaf: TerminalLeaf = { type: 'terminal', id: genId() }
  const root = ref<PaneNode>(initialLeaf)
  const focusedId = ref<string>(initialLeaf.id)

  // Maps paneId → PTY sessionId so sessions survive Vue remounts (e.g. splits)
  const ptySessionIds = ref<Record<string, string>>({})

  // Terminal display names
  const terminalNames = ref<Record<string, string>>({})

  function assignTerminalName(id: string) {
    terminalNames.value[id] = `Terminal ${nextTerminalNumber++}`
  }

  function getTerminalName(id: string): string {
    return terminalNames.value[id] ?? 'Terminal'
  }

  function setTerminalName(id: string, name: string) {
    terminalNames.value[id] = name
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
    return check(root.value)
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

    // Focus the leaf in the sibling closest to where the closed pane was:
    // closed right/bottom → focus the rightmost/bottommost leaf of left/top sibling
    // closed left/top → focus the leftmost/topmost leaf of right/bottom sibling
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

  // ── Serialization ────────────────────────────────────────────────────────
  function serializeLayout(): { layout: SavedPaneNode; terminals: { id: string; name: string }[]; focusedTerminalIndex: number } {
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

    const layout = walk(root.value)
    const focusedTerminalIndex = terminals.findIndex(t => t.id === focusedId.value)
    return { layout, terminals, focusedTerminalIndex: focusedTerminalIndex >= 0 ? focusedTerminalIndex : 0 }
  }

  function restoreFromSaved(saved: SavedPaneNode, terminals: SavedTerminal[], focusedTerminalIndex?: number) {
    // Reset counters
    nextId = 1
    nextTerminalNumber = 1
    terminalNames.value = {}
    ptySessionIds.value = {}
    savedCwds.value = {}
    savedClaudeSessions.value = {}
    savedClaudeWasRunning.value = {}

    // Track terminal IDs by their index so we can restore focus
    const terminalIdsByIndex: string[] = []

    function build(node: SavedPaneNode): PaneNode {
      if (node.type === 'terminal') {
        const id = genId()
        const t = terminals[node.index]
        terminalIdsByIndex[node.index] = id
        if (t) {
          terminalNames.value[id] = t.name
          // Track the highest terminal number to continue from there
          const match = t.name.match(/^Terminal (\d+)$/)
          if (match) {
            const n = parseInt(match[1], 10)
            if (n >= nextTerminalNumber) nextTerminalNumber = n + 1
          }
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
    root.value = newRoot

    // Restore focus to the saved terminal, or fall back to first leaf
    const restoredFocusId = focusedTerminalIndex != null ? terminalIdsByIndex[focusedTerminalIndex] : undefined
    if (restoredFocusId) {
      focusedId.value = restoredFocusId
    } else {
      function firstLeaf(node: PaneNode): string {
        if (node.type === 'terminal') return node.id
        return firstLeaf(node.first)
      }
      focusedId.value = firstLeaf(newRoot)
    }
  }

  return {
    root, focusedId, splitFocused, closeFocused, setFocus,
    updateSplitSizes, adjustSplitSize,
    setPtySession, getPtySession, hasPaneId, removePtySession,
    terminalNames, getTerminalName, setTerminalName, assignTerminalName,
    claudeSessionIds, setClaudeSessionId, clearClaudeSessionId, getClaudeSessionId, isClaudeRunning,
    savedCwds, savedClaudeSessions,
    getSavedCwd, consumeSavedCwd, getSavedClaudeSession, consumeSavedClaudeSession, consumeSavedClaudeWasRunning,
    focusTrigger, triggerFocus,
    serializeLayout, restoreFromSaved,
  }
})
