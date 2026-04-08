// ── Terminal workspace config (existing) ─────────────────────────────────────

export interface SavedTerminalWorkspace {
  type: 'terminal'
  name: string
  layout: SavedPaneNode
  terminals: SavedTerminal[]
  focusedTerminalIndex?: number
}

// ── Project workspace config (new) ──────────────────────────────────────────

export interface SavedWorktree {
  branchName: string
  path: string
  isMain: boolean
  parentBranch?: string | null           // Branch this was created from (null for main)
  claudePaneIndex: number                // Index into terminals[] for the unclosable Claude pane
  defaultTerminalIndex: number           // Index into terminals[] for the default shell
  layout: SavedPaneNode                  // Full center content tree
  terminals: SavedTerminal[]             // All terminals in this worktree
  explorerExpandedPaths?: string[]
}

export interface SavedProjectWorkspace {
  type: 'project'
  name: string
  repoRoot: string
  worktrees: SavedWorktree[]
  activeWorktreeId?: string
}

// ── Discriminated union ─────────────────────────────────────────────────────

export type SavedWorkspace = SavedTerminalWorkspace | SavedProjectWorkspace

// ── Legacy compat: old format without type field ────────────────────────────
// Used by restoreAllWorkspaces to handle configs saved before workspace types

export interface LegacySavedWorkspace {
  name: string
  layout: SavedPaneNode
  terminals: SavedTerminal[]
  focusedTerminalIndex?: number
}

// ── Top-level config ────────────────────────────────────────────────────────

export interface ArbiterConfig {
  window?: WindowGeometry
  overview?: WindowGeometry
  overviewVisible?: boolean
  // Multi-workspace (new format)
  workspaces?: SavedWorkspace[]
  activeWorkspaceIndex?: number
  // Legacy single-workspace (backward compat)
  layout?: SavedPaneNode
  terminals?: SavedTerminal[]
  focusedTerminalIndex?: number
}

export interface WindowGeometry {
  width: number
  height: number
  x: number
  y: number
}

export type SavedPaneNode =
  | { type: 'terminal'; index: number }
  | {
      type: 'split'
      direction: 'vertical' | 'horizontal'
      sizes: [number, number]
      first: SavedPaneNode
      second: SavedPaneNode
    }

export interface SavedTerminal {
  name: string
  cwd?: string
  claudeSessionId?: string
  claudeWasRunning?: boolean
  shell?: 'powershell' | 'gitbash'
}
