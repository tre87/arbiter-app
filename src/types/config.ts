export interface ArbiterConfig {
  closeOptions: CloseOptions
  window?: WindowGeometry
  layout?: SavedPaneNode
  terminals?: SavedTerminal[]
  focusedTerminalIndex?: number
}

export interface CloseOptions {
  saveLayout: boolean
  savePaths: boolean
  saveSessions: boolean
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
}
