import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

/**
 * Resolve when the PTY at `sessionId` reports OSC 133 prompt-idle, or when
 * the fallback timeout elapses. Replaces fixed setTimeout-after-create_session
 * timing for sending `claude` / `claude --resume` — on a cold start where the
 * shell hasn't printed its first prompt within 500 ms (slow profile load,
 * mid-`npm install`, etc.), a literal "claude" would otherwise be queued as
 * input to the shell-not-yet-ready and lost or misinterpreted.
 *
 * Returns immediately if the shell has already reported idle (via the cached
 * `get_session_shell_idle` state from the backend).
 */
export async function waitForShellIdle(sessionId: string, timeoutMs = 5000): Promise<void> {
  const current = await invoke<boolean | null>('get_session_shell_idle', { sessionId }).catch(() => null)
  if (current === true) return
  return new Promise((resolve) => {
    let done = false
    let unlistenFn: UnlistenFn | null = null
    const finish = () => {
      if (done) return
      done = true
      clearTimeout(timer)
      unlistenFn?.()
      resolve()
    }
    const timer = setTimeout(finish, timeoutMs)
    listen<boolean>(`shell-activity-${sessionId}`, (e) => {
      if (e.payload === true) finish()
    }).then((u) => {
      unlistenFn = u
      // If the transition fired between get_session_shell_idle and listen()
      // being attached, the caller will hit the timeout — acceptable; the
      // fallback unblocks within timeoutMs.
      if (done) u()
    }).catch(() => { /* listen failed — rely on timeout */ })
  })
}
