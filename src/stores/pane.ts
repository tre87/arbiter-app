import { defineStore } from 'pinia'
import { ref } from 'vue'
import type { PaneNode, TerminalLeaf } from '../types/pane'

let nextId = 1
const genId = () => String(nextId++)

export const usePaneStore = defineStore('pane', () => {
  const initialLeaf: TerminalLeaf = { type: 'terminal', id: genId() }
  const root = ref<PaneNode>(initialLeaf)
  const focusedId = ref<string>(initialLeaf.id)

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

  return { root, focusedId, splitFocused, setFocus, updateSplitSizes }
})
