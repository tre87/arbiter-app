import { defineStore } from 'pinia'
import { ref } from 'vue'

/** Lightweight perf telemetry for the debug footer, focused on terminal input
 *  latency. The focused terminal stamps each keystroke; its PTY-output listener
 *  computes the round-trip when the echo arrives. We also time the input-write
 *  IPC hop on its own so we can tell an input-side stall (invoke overhead) from
 *  an output-side one (echo return + parse + render). */
export const usePerfStore = defineStore('perf', () => {
  // input → echo round-trip (ms) per pane.
  const echoMs = ref<Record<string, number>>({})
  // write_to_session invoke duration (ms) per pane — input-side IPC only.
  const writeMs = ref<Record<string, number>>({})
  let pendingInput: { paneId: string; at: number } | null = null

  function markInput(paneId: string) {
    pendingInput = { paneId, at: performance.now() }
  }

  /** Called from a pane's output listener; records round-trip if an input for
   *  THIS pane is awaiting its echo. */
  function markOutput(paneId: string) {
    if (pendingInput && pendingInput.paneId === paneId) {
      echoMs.value[paneId] = performance.now() - pendingInput.at
      pendingInput = null
    }
  }

  function recordWrite(paneId: string, ms: number) {
    writeMs.value[paneId] = ms
  }

  return { echoMs, writeMs, markInput, markOutput, recordWrite }
})
