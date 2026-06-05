<script setup lang="ts">
// One transparent WebGL canvas floating over the whole window. Mounted only
// when the GPU renderer is enabled (devSettings.useGpuRenderer). It composites
// the terminal grids over the DOM: panes register via useTerminalGrid and the
// draw loop paints each visible pane's grid at its terminal-content rect.
// pointer-events:none so focus/clicks pass through to the (hidden) xterm input
// layer underneath.
import { ref, onMounted, onBeforeUnmount } from 'vue'
import { initTerminalCanvas, teardownTerminalCanvas } from '../composables/useTerminalGrid'

const canvasEl = ref<HTMLCanvasElement>()

onMounted(() => { if (canvasEl.value) initTerminalCanvas(canvasEl.value) })
onBeforeUnmount(() => teardownTerminalCanvas())
</script>

<template>
  <!-- Inline z-index beats App's `.app > * { z-index: 1 }` (same specificity)
       so the canvas sits above the panes; dialogs at z-index 9999 stay on top. -->
  <canvas ref="canvasEl" class="shared-terminal-canvas" :style="{ zIndex: 10 }" />
</template>

<style scoped>
.shared-terminal-canvas {
  position: fixed;
  inset: 0;
  width: 100%;
  height: 100%;
  pointer-events: none;
}
</style>
