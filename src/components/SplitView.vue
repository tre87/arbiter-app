<script setup lang="ts">
import { ref } from 'vue'
import type { PaneNode, SplitNode } from '../types/pane'
import TerminalPane from './TerminalPane.vue'
import { usePaneStore } from '../stores/pane'

defineOptions({ name: 'SplitView' })

// Cap recursion at a sane ceiling so a corrupt saved layout (or a runaway
// split storm) can't blow the Vue component stack. 20 nested splits = 2^20
// theoretical leaves, far past anything a user could resolve on screen.
const MAX_DEPTH = 20

const props = withDefaults(defineProps<{ node: PaneNode; depth?: number }>(), { depth: 0 })
const store = usePaneStore()

if (props.depth > MAX_DEPTH) {
  console.error(`SplitView depth exceeded ${MAX_DEPTH}; refusing to render deeper.`)
}
const containerRef = ref<HTMLDivElement>()
const isDragging = ref(false)

function startDrag(e: MouseEvent) {
  if (props.node.type !== 'split') return
  e.preventDefault()

  const node = props.node as SplitNode
  const rect = containerRef.value!.getBoundingClientRect()
  const isVertical = node.direction === 'vertical'
  isDragging.value = true

  const onMove = (ev: MouseEvent) => {
    const pos = isVertical ? ev.clientX - rect.left : ev.clientY - rect.top
    const total = isVertical ? rect.width : rect.height
    // Pixel-based minimum: 100px width, 80px height
    const minPx = isVertical ? 100 : 80
    const minPct = Math.max(5, (minPx / total) * 100)
    const pct = Math.max(minPct, Math.min(100 - minPct, (pos / total) * 100))
    store.updateSplitSizes(node.id, [pct, 100 - pct])
  }

  const onUp = () => {
    isDragging.value = false
    window.removeEventListener('mousemove', onMove)
    window.removeEventListener('mouseup', onUp)
  }

  window.addEventListener('mousemove', onMove)
  window.addEventListener('mouseup', onUp)
}
</script>

<template>
  <!-- Terminal leaf -->
  <TerminalPane v-if="node.type === 'terminal'" :pane-id="node.id" />

  <!-- Depth ceiling reached — render nothing rather than recurse further. -->
  <template v-else-if="depth > MAX_DEPTH" />

  <!-- Split node -->
  <div
    v-else
    ref="containerRef"
    class="split"
    :class="node.direction"
  >
    <!-- Drag overlay prevents terminals stealing mouse during resize -->
    <div v-if="isDragging" class="drag-overlay" />

    <div class="split-child" :style="{ flex: node.sizes[0] }">
      <SplitView :node="node.first" :depth="depth + 1" />
    </div>

    <div
      class="divider"
      :class="node.direction"
      @mousedown="startDrag"
    />

    <div class="split-child" :style="{ flex: node.sizes[1] }">
      <SplitView :node="node.second" :depth="depth + 1" />
    </div>
  </div>
</template>

<style scoped>
.split {
  display: flex;
  width: 100%;
  height: 100%;
  overflow: hidden;
  position: relative;
}

.split.vertical  { flex-direction: row; }
.split.horizontal { flex-direction: column; }

.split-child {
  overflow: hidden;
  min-width: 0;
  min-height: 0;
}

.divider {
  flex-shrink: 0;
  background: var(--color-card-border);
  transition: background 0.15s;
  z-index: 1;
}

.divider:hover,
.divider:active {
  background: var(--color-accent);
}

.divider.vertical {
  width: 2px;
  cursor: col-resize;
}

.divider.horizontal {
  height: 2px;
  cursor: row-resize;
}

.drag-overlay {
  position: absolute;
  inset: 0;
  z-index: 100;
  cursor: inherit;
}
</style>
