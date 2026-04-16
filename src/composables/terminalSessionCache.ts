import type { Ref } from 'vue'
import type { UnlistenFn } from '@tauri-apps/api/event'
import type { XtermInstance } from './useXtermInstance'

/** A live terminal session that outlives the Vue component mount.
 *
 *  VS Code's terminal does the same thing: the xterm.js Terminal is created
 *  once per logical session and stays alive for its entire lifetime. When the
 *  visible tab/pane changes, only the wrapper DOM element is detached and
 *  reattached — the Terminal instance, its scrollback, and its event handlers
 *  all persist. That's why switching between their terminals never flickers.
 *
 *  In Arbiter this lets us keep `v-if` on worktrees (so only one pane tree is
 *  mounted at a time) while avoiding the replay-and-repaint flicker that the
 *  old `dispose-on-unmount` model forced. */
export interface TerminalSession {
  xterm: XtermInstance
  /** Detachable DOM wrapper that hosts xterm's own element. We append this
   *  into whatever container the currently-mounted TerminalPane gives us. */
  wrapperEl: HTMLDivElement
  /** Current PTY session id. Handlers registered once at xterm-creation time
   *  read this through the session object so shell-switch updates propagate. */
  sessionId: string | null
  /** Tauri unlisten for the current `pty-output-{sessionId}` subscription.
   *  Replaced when the session id changes (switchShell). */
  ptyUnlisten: UnlistenFn | null
  /** Reactive refs shared with whichever TerminalPane is currently mounted.
   *  Keeping them on the session means state like the OSC 0 title or the
   *  active shell survives Vue remounts. */
  title: Ref<string>
  shell: Ref<'powershell' | 'gitbash'>
}

const cache = new Map<string, TerminalSession>()

export function getTerminalSession(paneId: string): TerminalSession | undefined {
  return cache.get(paneId)
}

export function setTerminalSession(paneId: string, session: TerminalSession): void {
  cache.set(paneId, session)
}

export function disposeTerminalSession(paneId: string): void {
  const session = cache.get(paneId)
  if (!session) return
  session.ptyUnlisten?.()
  session.xterm.dispose()
  session.wrapperEl.remove()
  cache.delete(paneId)
}
