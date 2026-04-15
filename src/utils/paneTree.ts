import type { PaneNode, Workspace } from '../types/pane'

export function nextAvailableNumber(prefix: string, usedNames: Iterable<string>): number {
  const used = new Set<number>()
  for (const name of usedNames) {
    const match = name.match(new RegExp(`^${prefix} (\\d+)$`))
    if (match) used.add(parseInt(match[1], 10))
  }
  let n = 1
  while (used.has(n)) n++
  return n
}

export function getWorkspaceRoot(ws: Workspace): PaneNode {
  if (ws.type === 'project') {
    const wt = ws.worktrees.find(w => w.id === ws.activeWorktreeId)
    return wt ? wt.root : ws.worktrees[0].root
  }
  return ws.root
}

export function setWorkspaceRoot(ws: Workspace, val: PaneNode) {
  if (ws.type === 'project') {
    const wt = ws.worktrees.find(w => w.id === ws.activeWorktreeId)
    if (wt) wt.root = val
  } else {
    ws.root = val
  }
}

export function getWorkspaceFocusedId(ws: Workspace): string {
  return ws.type === 'project' ? ws.focusedPaneId : ws.focusedId
}

export function setWorkspaceFocusedId(ws: Workspace, val: string) {
  if (ws.type === 'project') {
    ws.focusedPaneId = val
  } else {
    ws.focusedId = val
  }
}

export function collectPaneIdsFromNode(node: PaneNode): string[] {
  if (node.type === 'terminal') return [node.id]
  return [...collectPaneIdsFromNode(node.first), ...collectPaneIdsFromNode(node.second)]
}

export function collectAllPaneIds(ws: Workspace): string[] {
  if (ws.type === 'project') {
    return ws.worktrees.flatMap(wt => collectPaneIdsFromNode(wt.root))
  }
  return collectPaneIdsFromNode(ws.root)
}

export function nodeContainsId(node: PaneNode, id: string): boolean {
  if (node.type === 'terminal') return node.id === id
  return nodeContainsId(node.first, id) || nodeContainsId(node.second, id)
}

export function firstLeaf(node: PaneNode): string {
  if (node.type === 'terminal') return node.id
  return firstLeaf(node.first)
}

export function lastLeaf(node: PaneNode): string {
  if (node.type === 'terminal') return node.id
  return lastLeaf(node.second)
}
