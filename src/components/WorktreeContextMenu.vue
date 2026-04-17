<script setup lang="ts">
import { onMounted, onBeforeUnmount, nextTick, ref, watch } from 'vue'
import MdiIcon from './MdiIcon.vue'
import { mdiSourceMerge, mdiRobotOutline, mdiClose, mdiDeleteOutline, mdiDeleteAlertOutline, mdiSourcePull, mdiRefresh } from '@mdi/js'
import type { Worktree } from '../types/pane'

const props = defineProps<{
  worktree: Worktree
  clickX: number
  clickY: number
  isMain: boolean
  isMerged: boolean
  canAskClaude: boolean
  mainBranch: string
}>()

const emit = defineEmits<{
  (e: 'close'): void
  (e: 'manualMerge'): void
  (e: 'claudeMerge'): void
  (e: 'mergeAndDelete'): void
  (e: 'createPr'): void
  (e: 'delete'): void
  (e: 'discard'): void
  (e: 'dismissMerged'): void
  (e: 'regenerateRobot'): void
}>()

const pos = ref({ x: props.clickX, y: props.clickY })
const menuEl = ref<HTMLElement | null>(null)

// Position the menu so it fits within the viewport, using the click point as anchor.
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
  if (!target.closest('.worktree-context-menu')) emit('close')
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
      class="worktree-context-menu"
      :style="{ left: pos.x + 'px', top: pos.y + 'px' }"
    >
      <template v-if="isMain">
        <div class="ctx-section">
          <button class="ctx-item" @click="emit('regenerateRobot')">
            <MdiIcon :path="mdiRefresh" :size="14" />
            <span>Regenerate robot</span>
          </button>
        </div>
      </template>
      <template v-else-if="isMerged">
        <div class="ctx-section">
          <button class="ctx-item" @click="emit('dismissMerged')">
            <MdiIcon :path="mdiClose" :size="14" />
            <span>Dismiss merged worktree</span>
          </button>
          <button class="ctx-item" @click="emit('regenerateRobot')">
            <MdiIcon :path="mdiRefresh" :size="14" />
            <span>Regenerate robot</span>
          </button>
        </div>
      </template>
      <template v-else>
        <div v-if="worktree.parentBranch" class="ctx-section">
          <button
            class="ctx-item"
            :disabled="!worktree.parentBranch"
            @click="emit('manualMerge')"
          >
            <MdiIcon :path="mdiSourceMerge" :size="14" />
            <span>Merge into <b>{{ worktree.parentBranch }}</b></span>
          </button>
          <button
            class="ctx-item"
            :disabled="!canAskClaude"
            :title="!canAskClaude ? 'Parent worktree is busy or not open' : ''"
            @click="emit('claudeMerge')"
          >
            <MdiIcon :path="mdiRobotOutline" :size="14" />
            <span>Ask Claude to merge into <b>{{ worktree.parentBranch }}</b></span>
          </button>
        </div>
        <div v-else class="ctx-empty">No parent branch recorded</div>
        <div class="ctx-section">
          <button class="ctx-item" @click="emit('mergeAndDelete')">
            <MdiIcon :path="mdiSourceMerge" :size="14" />
            <span>Merge into <b>{{ mainBranch }}</b> &amp; delete</span>
          </button>
          <button class="ctx-item" @click="emit('createPr')">
            <MdiIcon :path="mdiSourcePull" :size="14" />
            <span>Push, create PR &amp; delete</span>
          </button>
        </div>
        <div class="ctx-section">
          <button class="ctx-item" @click="emit('regenerateRobot')">
            <MdiIcon :path="mdiRefresh" :size="14" />
            <span>Regenerate robot</span>
          </button>
        </div>
        <div class="ctx-section">
          <button class="ctx-item" @click="emit('delete')">
            <MdiIcon :path="mdiDeleteOutline" :size="14" />
            <span>Delete worktree</span>
          </button>
          <button class="ctx-item ctx-danger" @click="emit('discard')">
            <MdiIcon :path="mdiDeleteAlertOutline" :size="14" />
            <span>Discard changes</span>
          </button>
        </div>
      </template>
    </div>
  </Teleport>
</template>

<style scoped>
.worktree-context-menu {
  position: fixed;
  z-index: 2000;
  min-width: 240px;
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
.ctx-empty {
  padding: 8px 12px;
  color: var(--color-text-muted);
}
</style>
