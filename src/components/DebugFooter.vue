<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount } from 'vue'
import { usePaneStore } from '../stores/pane'
import { usePerfStore } from '../stores/perf'
import { getTerminalSession, sessionCount } from '../composables/terminalSessionCache'
import { activeWebglCount } from '../composables/useXtermInstance'

const store = usePaneStore()
const perf = usePerfStore()

// Render-loop health, sampled every 500ms.
const fps = ref(0)
const jank = ref(0) // worst frame gap (ms) in the window — main-thread stalls
const terms = ref(0)
const webgl = ref(0)
const heapMb = ref<number | null>(null)

let raf = 0
let frames = 0
let windowStart = performance.now()
let lastFrame = performance.now()
let maxGap = 0

function tick(t: number) {
  frames++
  const gap = t - lastFrame
  if (gap > maxGap) maxGap = gap
  lastFrame = t
  if (t - windowStart >= 500) {
    fps.value = Math.round((frames * 1000) / (t - windowStart))
    jank.value = Math.round(maxGap)
    terms.value = sessionCount()
    webgl.value = activeWebglCount()
    const mem = (performance as unknown as { memory?: { usedJSHeapSize: number } }).memory
    heapMb.value = mem ? Math.round(mem.usedJSHeapSize / 1048576) : null
    frames = 0
    maxGap = 0
    windowStart = t
  }
  raf = requestAnimationFrame(tick)
}

onMounted(() => { raf = requestAnimationFrame(tick) })
onBeforeUnmount(() => cancelAnimationFrame(raf))

// Focused-terminal specifics (the one you're typing in).
const focused = computed(() => getTerminalSession(store.focusedId))
const renderer = computed(() => (focused.value ? (focused.value.xterm.hasWebgl() ? 'GPU' : 'DOM') : '—'))
const size = computed(() => (focused.value ? `${focused.value.xterm.term.cols}×${focused.value.xterm.term.rows}` : '—'))
const scrollback = computed(() => focused.value?.xterm.term.options.scrollback ?? '—')
const echo = computed(() => { const v = perf.echoMs[store.focusedId]; return v != null ? Math.round(v) : null })
const write = computed(() => { const v = perf.writeMs[store.focusedId]; return v != null ? Math.round(v) : null })
</script>

<template>
  <div class="debug-footer">
    <span class="grp" :class="{ warn: fps > 0 && fps < 50 }">FPS {{ fps }}</span>
    <span class="grp" :class="{ warn: jank > 32 }">jank {{ jank }}ms</span>

    <!-- GPU single-canvas renderer: parse-in-Rust → binary diff → 1 canvas. -->
    <template v-if="perf.gpuActive">
      <span class="sep">|</span>
      <span class="grp lbl">gpu</span>
      <span class="grp">{{ perf.gpuFramesPerSec }} frm/s</span>
      <span class="grp">{{ perf.gpuKbPerSec }} KB/s</span>
      <span class="grp" :class="{ warn: perf.gpuDecodeMs > 4 }">dec {{ perf.gpuDecodeMs }}ms</span>
      <span class="grp" :class="{ warn: perf.gpuDrawMs > 6 }">draw {{ perf.gpuDrawMs }}ms</span>
      <span class="sep">|</span>
      <span class="grp">terms {{ terms }}</span>
      <span v-if="heapMb !== null" class="grp">heap {{ heapMb }}MB</span>
    </template>

    <!-- Legacy per-terminal xterm path. -->
    <template v-else>
      <span class="sep">|</span>
      <span class="grp lbl">input</span>
      <span class="grp" :class="{ warn: echo != null && echo > 25 }">echo {{ echo ?? '—' }}ms</span>
      <span class="grp">write {{ write ?? '—' }}ms</span>
      <span class="sep">|</span>
      <span class="grp lbl">focus</span>
      <span class="grp" :class="{ warn: renderer === 'DOM' }">{{ renderer }}</span>
      <span class="grp">{{ size }}</span>
      <span class="grp">sb {{ scrollback }}</span>
      <span class="sep">|</span>
      <span class="grp">terms {{ terms }}</span>
      <span class="grp" :class="{ warn: webgl > 12 }">gl {{ webgl }}</span>
      <span v-if="heapMb !== null" class="grp">heap {{ heapMb }}MB</span>
    </template>

    <span class="spacer" />
    <span class="grp hint">Ctrl/Cmd+Shift+P</span>
  </div>
</template>

<style scoped>
.debug-footer {
  display: flex;
  align-items: center;
  gap: 8px;
  height: 22px;
  padding: 0 10px;
  background: #0a0a0a;
  border-top: 1px solid var(--color-card-border);
  font-family: Consolas, 'Cascadia Code', Menlo, 'SF Mono', monospace;
  font-size: 11px;
  color: #9cdcfe;
  white-space: nowrap;
  overflow: hidden;
  user-select: none;
  flex-shrink: 0;
}
.grp { color: #9cdcfe; }
.grp.lbl { color: var(--color-text-muted); text-transform: uppercase; font-size: 9px; letter-spacing: 0.06em; }
.grp.warn { color: #e5a03c; font-weight: 600; }
.grp.hint { color: var(--color-text-muted); opacity: 0.6; }
.sep { color: var(--color-card-border); }
.spacer { flex: 1; }
</style>
