import type { PaneNode, SplitNode } from '../types/pane'

export interface Rect {
  x: number
  y: number
  w: number
  h: number
}

export type Direction = 'left' | 'right' | 'up' | 'down'

/**
 * Compute a normalized bounding box (0–1) for every terminal leaf in the tree.
 */
export function computeLeafRects(root: PaneNode): Map<string, Rect> {
  const rects = new Map<string, Rect>()

  function walk(node: PaneNode, x: number, y: number, w: number, h: number) {
    if (node.type === 'terminal') {
      rects.set(node.id, { x, y, w, h })
      return
    }
    const r = node.sizes[0] / 100
    if (node.direction === 'vertical') {
      walk(node.first, x, y, w * r, h)
      walk(node.second, x + w * r, y, w * (1 - r), h)
    } else {
      walk(node.first, x, y, w, h * r)
      walk(node.second, x, y + h * r, w, h * (1 - r))
    }
  }

  walk(root, 0, 0, 1, 1)
  return rects
}

const EPS = 1e-6

function overlaps(a0: number, a1: number, b0: number, b1: number): boolean {
  return a0 < b1 - EPS && b0 < a1 - EPS
}

/**
 * Find the neighbor leaf in a given direction from the focused leaf.
 */
export function findNeighbor(
  rects: Map<string, Rect>,
  focusedId: string,
  direction: Direction,
): string | null {
  const f = rects.get(focusedId)
  if (!f) return null

  let best: string | null = null
  let bestDist = Infinity

  for (const [id, r] of rects) {
    if (id === focusedId) continue

    let isCandidate = false
    let dist = Infinity

    switch (direction) {
      case 'right':
        isCandidate = r.x >= f.x + f.w - EPS && overlaps(f.y, f.y + f.h, r.y, r.y + r.h)
        dist = r.x - (f.x + f.w)
        break
      case 'left':
        isCandidate = r.x + r.w <= f.x + EPS && overlaps(f.y, f.y + f.h, r.y, r.y + r.h)
        dist = f.x - (r.x + r.w)
        break
      case 'down':
        isCandidate = r.y >= f.y + f.h - EPS && overlaps(f.x, f.x + f.w, r.x, r.x + r.w)
        dist = r.y - (f.y + f.h)
        break
      case 'up':
        isCandidate = r.y + r.h <= f.y + EPS && overlaps(f.x, f.x + f.w, r.x, r.x + r.w)
        dist = f.y - (r.y + r.h)
        break
    }

    if (isCandidate && dist < bestDist) {
      bestDist = dist
      best = id
    }
  }

  return best
}

export interface PathEntry {
  splitId: string
  child: 'first' | 'second'
}

/**
 * Get the path from root to a leaf node.
 */
export function getPathToLeaf(root: PaneNode, leafId: string): PathEntry[] | null {
  function walk(node: PaneNode): PathEntry[] | null {
    if (node.type === 'terminal') {
      return node.id === leafId ? [] : null
    }
    const firstPath = walk(node.first)
    if (firstPath !== null) {
      return [{ splitId: node.id, child: 'first' }, ...firstPath]
    }
    const secondPath = walk(node.second)
    if (secondPath !== null) {
      return [{ splitId: node.id, child: 'second' }, ...secondPath]
    }
    return null
  }
  return walk(root)
}

/**
 * Find the nearest ancestor split that can be resized in the given direction.
 * Returns the splitId and the delta sign (+1 to grow sizes[0], -1 to shrink).
 */
export function findResizableSplit(
  root: PaneNode,
  focusedId: string,
  direction: Direction,
): { splitId: string; delta: number } | null {
  const path = getPathToLeaf(root, focusedId)
  if (!path) return null

  // Walk bottom-up
  for (let i = path.length - 1; i >= 0; i--) {
    const entry = path[i]
    // Find the split node to check its direction
    const splitNode = findSplitById(root, entry.splitId)
    if (!splitNode) continue

    if (splitNode.direction === 'vertical' && (direction === 'left' || direction === 'right')) {
      // first child: right grows, left shrinks
      // second child: left grows, right shrinks
      const delta = entry.child === 'first'
        ? (direction === 'right' ? 1 : -1)
        : (direction === 'left' ? -1 : 1)
      return { splitId: entry.splitId, delta }
    }
    if (splitNode.direction === 'horizontal' && (direction === 'up' || direction === 'down')) {
      // first child: down grows, up shrinks
      // second child: up grows, down shrinks
      const delta = entry.child === 'first'
        ? (direction === 'down' ? 1 : -1)
        : (direction === 'up' ? -1 : 1)
      return { splitId: entry.splitId, delta }
    }
  }

  return null
}

function findSplitById(node: PaneNode, id: string): SplitNode | null {
  if (node.type === 'split') {
    if (node.id === id) return node
    return findSplitById(node.first, id) ?? findSplitById(node.second, id)
  }
  return null
}
