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

  // GPU single-canvas renderer telemetry (when useGpuRenderer is on). Populated
  // by useTerminalGrid; surfaced in the debug footer in place of the per-xterm
  // echo/write/gl readings (which don't apply when xterm isn't rendering).
  const gpuActive = ref(false)
  const gpuFramesPerSec = ref(0) // binary diff frames received from Rust / sec
  const gpuKbPerSec = ref(0)     // transport throughput
  const gpuDecodeMs = ref(0)     // main-thread decode cost per frame
  const gpuDrawMs = ref(0)       // main-thread build + draw cost per frame

  function setGpuActive(active: boolean) {
    gpuActive.value = active
  }
  function setGpuStats(s: { framesPerSec: number; kbPerSec: number; decodeMs: number; drawMs: number }) {
    gpuFramesPerSec.value = s.framesPerSec
    gpuKbPerSec.value = s.kbPerSec
    gpuDecodeMs.value = s.decodeMs
    gpuDrawMs.value = s.drawMs
  }

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

  return {
    echoMs, writeMs, markInput, markOutput, recordWrite,
    gpuActive, gpuFramesPerSec, gpuKbPerSec, gpuDecodeMs, gpuDrawMs, setGpuActive, setGpuStats,
  }
})
