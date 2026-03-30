export interface TerminalLeaf {
  type: 'terminal'
  id: string
}

export interface SplitNode {
  type: 'split'
  id: string
  direction: 'vertical' | 'horizontal'
  sizes: [number, number]
  first: PaneNode
  second: PaneNode
}

export type PaneNode = TerminalLeaf | SplitNode

export interface Workspace {
  id: string
  name: string
  root: PaneNode
  focusedId: string
}
