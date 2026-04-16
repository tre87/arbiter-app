import { ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'

/** Shared across every TerminalPane. `check_git_bash` probes the filesystem
 *  and the answer is stable for the lifetime of the process, so we invoke it
 *  at most once — subsequent callers re-await the same promise. */
export const gitBashPath = ref<string | null>(null)

let probePromise: Promise<string | null> | null = null

export function ensureGitBashProbed(): Promise<string | null> {
  if (probePromise) return probePromise
  probePromise = invoke<string | null>('check_git_bash').then((path) => {
    gitBashPath.value = path
    return path
  })
  return probePromise
}
