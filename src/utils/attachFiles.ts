import { invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'
import { usePaneStore } from '../stores/pane'

/** Write file paths to a specific PTY using bracketed paste mode
 *  (ESC[200~ … ESC[201~). Claude Code's Ink TUI only converts dropped/pasted
 *  paths into file attachments when they arrive inside this sequence — plain
 *  `write` lands as typed prompt text and the attachment never materialises.
 *
 *  Each path is sent as its own bracketed-paste sequence: Claude treats the
 *  full content of one paste as a single path string (so multiple paths
 *  space-joined inside one paste only register the first), but back-to-back
 *  pastes are processed independently and each becomes a separate attachment. */
export function writePathsToPane(paneId: string, paths: string[]): boolean {
  if (!paths.length) return false
  const store = usePaneStore()
  const sessionId = store.getPtySession(paneId)
  if (!sessionId) return false
  const payload = paths.map(p => `\x1b[200~${p}\x1b[201~`).join('')
  invoke('write_to_session', { sessionId, data: payload })
  return true
}

export function writePathsToFocusedPane(paths: string[]): boolean {
  const store = usePaneStore()
  return writePathsToPane(store.focusedId, paths)
}

/** Restore focus to the focused terminal after a native dialog steals it.
 *  Mirrors App.vue's startup focus dance: push native focus into the web
 *  content layer first (Windows WebView2 quirk), then focus xterm's textarea. */
export async function refocusActivePane() {
  try { await invoke('focus_webview') } catch { /* best-effort */ }
  const pane = document.querySelector('.terminal-pane.focused')
  const textarea = pane?.querySelector('textarea') as HTMLTextAreaElement | null
  textarea?.focus()
}

/** Returns the directory portion of an absolute file path (handles both \ and /). */
export function dirnameOf(p: string): string {
  const idx = Math.max(p.lastIndexOf('\\'), p.lastIndexOf('/'))
  return idx > 0 ? p.slice(0, idx) : p
}

/** Open the native file picker rooted at `defaultPath`, then paste the
 *  selected paths into the focused pane. Returns the chosen paths (empty on
 *  cancel / no focused pane). */
export async function pickAndAttach(defaultPath: string): Promise<string[]> {
  const selected = await open({ multiple: true, defaultPath })
  if (!selected) {
    refocusActivePane()
    return []
  }
  const paths = Array.isArray(selected) ? selected : [selected]
  if (!paths.length) {
    refocusActivePane()
    return []
  }
  writePathsToFocusedPane(paths)
  refocusActivePane()
  return paths
}
