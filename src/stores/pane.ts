import { defineStore } from 'pinia'
import { ref } from 'vue'
import type { PaneNode, TerminalLeaf } from '../types/pane'

let nextId = 1
const genId = () => String(nextId++)

export const usePaneStore = defineStore('pane', () => {
  const initialLeaf: TerminalLeaf = { type: 'terminal', id: genId() }
  const root = ref<PaneNode>(initialLeaf)
  const focusedId = ref<string>(initialLeaf.id)

  // Maps paneId → PTY sessionId so sessions survive Vue remounts (e.g. splits)
  const ptySessionIds = ref<Record<string, string>>({})

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

  return {
    root, focusedId, splitFocused, closeFocused, setFocus, updateSplitSizes,
    setPtySession, getPtySession, hasPaneId, removePtySession,
  }
})
