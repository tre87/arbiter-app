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

// ── Worktree types (for project workspaces) ─────────────────────────────────

export interface Worktree {
  id: string
  branchName: string
  path: string                          // Absolute worktree directory
  isMain: boolean
  parentBranch: string | null           // Branch this was created from (null for main)
  claudePaneId: string                  // Primary Claude terminal (unclosable)
  defaultTerminalPaneId: string         // Shell terminal below Claude
  root: PaneNode                        // Full center content tree (Claude + terminal + any extra splits)
  explorerExpandedPaths: string[]       // Persisted folder expand state
}

// ── Workspace types (discriminated union) ────────────────────────────────────

export interface TerminalWorkspace {
  type: 'terminal'
  id: string
  name: string
  root: PaneNode
  focusedId: string
}

export interface ProjectWorkspace {
  type: 'project'
  id: string
  name: string
  repoRoot: string
  worktrees: Worktree[]
  activeWorktreeId: string
  focusedPaneId: string
}

export type Workspace = TerminalWorkspace | ProjectWorkspace
