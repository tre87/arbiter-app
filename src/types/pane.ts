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

// ── Claude lifecycle state (centralized in pane store) ──────────────────────

export interface ClaudePaneState {
  /** closed = no Claude | launching = typed command, waiting for JSONL | ready = idle | working = active turn | attention = BEL */
  lifecycle: 'closed' | 'launching' | 'ready' | 'working' | 'attention'
  /** Non-empty once JSONL watcher confirms Claude. Null before confirmation or after exit. */
  sessionId: string | null
  /** True once backend confirmed Claude via JSONL (claude-started event). */
  confirmed: boolean
  model: string | null
  inputTokens: number
  outputTokens: number
  cacheReadTokens: number
  cacheWriteTokens: number
  contextPercent: number
  /** Real context window for the session (200k / 1M), from Claude's statusLine capture. Null until a capture arrives. */
  contextWindowSize: number | null
  /** Claude's own context used-% (input-side, output excluded). Null until a capture arrives. */
  usedPercentage: number | null
  /** True once a Tier-2 statusLine capture has been received for this session. */
  hasContext: boolean
  /** Estimated session cost in USD, computed from token counts + model pricing */
  cost: number
}
