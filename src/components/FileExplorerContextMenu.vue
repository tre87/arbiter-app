<script setup lang="ts">
import { onMounted, onBeforeUnmount, nextTick, ref, watch } from 'vue'
import MdiIcon from './MdiIcon.vue'
import { mdiOpenInApp, mdiFolderOpenOutline, mdiPencilOutline, mdiDeleteOutline } from '@mdi/js'

const props = defineProps<{
  clickX: number
  clickY: number
  selectionCount: number
  allFiles: boolean
  revealLabel: string
}>()

const emit = defineEmits<{
  (e: 'close'): void
  (e: 'open'): void
  (e: 'reveal'): void
  (e: 'rename'): void
  (e: 'delete'): void
}>()

const pos = ref({ x: props.clickX, y: props.clickY })
const menuEl = ref<HTMLElement | null>(null)

watch(() => [props.clickX, props.clickY], () => {
  pos.value = { x: props.clickX, y: props.clickY }
  nextTick(() => reposition())
}, { immediate: false })

function reposition() {
  const el = menuEl.value
  if (!el) return
  const rect = el.getBoundingClientRect()
  const margin = 8
  const vw = window.innerWidth
  const vh = window.innerHeight

  let x = props.clickX
  if (props.clickX + rect.width + margin > vw) x = props.clickX - rect.width
  x = Math.max(margin, Math.min(x, vw - rect.width - margin))

  let y = props.clickY
  if (props.clickY + rect.height + margin > vh) y = props.clickY - rect.height
  y = Math.max(margin, Math.min(y, vh - rect.height - margin))

  pos.value = { x, y }
}

function onWindowMouseDown(e: MouseEvent) {
  const target = e.target as HTMLElement
  if (!target.closest('.file-explorer-context-menu')) emit('close')
}

onMounted(() => {
  document.addEventListener('mousedown', onWindowMouseDown)
  nextTick(reposition)
})
onBeforeUnmount(() => document.removeEventListener('mousedown', onWindowMouseDown))
</script>

<template>
  <Teleport to="body">
    <div
      ref="menuEl"
      class="file-explorer-context-menu"
      :style="{ left: pos.x + 'px', top: pos.y + 'px' }"
    >
      <div class="ctx-section">
        <button v-if="allFiles" class="ctx-item" @click="emit('open')">
          <MdiIcon :path="mdiOpenInApp" :size="14" />
          <span>{{ selectionCount > 1 ? `Open ${selectionCount} files` : 'Open' }}</span>
        </button>
        <button
          class="ctx-item"
          :disabled="selectionCount !== 1"
          @click="selectionCount === 1 && emit('reveal')"
        >
          <MdiIcon :path="mdiFolderOpenOutline" :size="14" />
          <span>{{ revealLabel }}</span>
        </button>
      </div>
      <div class="ctx-section">
        <button
          class="ctx-item"
          :disabled="selectionCount !== 1"
          @click="selectionCount === 1 && emit('rename')"
        >
          <MdiIcon :path="mdiPencilOutline" :size="14" />
          <span>Rename</span>
        </button>
        <button class="ctx-item ctx-danger" @click="emit('delete')">
          <MdiIcon :path="mdiDeleteOutline" :size="14" />
          <span>{{ selectionCount > 1 ? `Delete ${selectionCount} items` : 'Delete' }}</span>
        </button>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
.file-explorer-context-menu {
  position: fixed;
  z-index: 2000;
  min-width: 200px;
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-card-border);
  border-radius: 6px;
  box-shadow: 0 6px 18px rgba(0, 0, 0, 0.45);
  padding: 4px 0;
  font-size: 12px;
  color: var(--color-text-primary);
}
.ctx-item {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  background: none;
  border: none;
  color: inherit;
  text-align: left;
  padding: 6px 12px;
  cursor: pointer;
  font: inherit;
}
.ctx-item:hover:not(:disabled) {
  background: rgba(255, 255, 255, 0.06);
}
.ctx-item:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}
.ctx-item.ctx-danger { color: var(--color-danger); }
.ctx-section + .ctx-section {
  border-top: 1px solid var(--color-card-border);
  margin-top: 4px;
  padding-top: 4px;
}
</style>
