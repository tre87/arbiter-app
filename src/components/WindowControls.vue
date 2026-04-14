<script setup lang="ts">
import { ref, onMounted, onBeforeUnmount } from 'vue'
import { getCurrentWindow } from '@tauri-apps/api/window'

const isMaximized = ref(false)
let unlisten: (() => void) | null = null

async function syncMaximized() {
  isMaximized.value = await getCurrentWindow().isMaximized()
}

onMounted(async () => {
  await syncMaximized()
  unlisten = await getCurrentWindow().onResized(syncMaximized)
})

onBeforeUnmount(() => {
  unlisten?.()
})

function minimize() {
  getCurrentWindow().minimize()
}

function toggleMaximize() {
  getCurrentWindow().toggleMaximize()
}

function close() {
  getCurrentWindow().close()
}
</script>

<template>
  <div class="window-controls">
    <button class="win-btn minimize" title="Minimize" @click="minimize">
      <svg width="10" height="1" viewBox="0 0 10 1"><rect width="10" height="1" fill="currentColor" /></svg>
    </button>
    <button class="win-btn maximize" title="Maximize" @click="toggleMaximize">
      <svg v-if="!isMaximized" width="10" height="10" viewBox="0 0 10 10">
        <rect x="0" y="0" width="10" height="10" rx="0" fill="none" stroke="currentColor" stroke-width="1" />
      </svg>
      <svg v-else width="10" height="10" viewBox="0 0 10 10">
        <rect x="2" y="0" width="8" height="8" rx="0" fill="none" stroke="currentColor" stroke-width="1" />
        <rect x="0" y="2" width="8" height="8" rx="0" fill="var(--color-bg-subtle)" stroke="currentColor" stroke-width="1" />
      </svg>
    </button>
    <button class="win-btn close" title="Close" @click="close">
      <svg width="10" height="10" viewBox="0 0 10 10">
        <line x1="0" y1="0" x2="10" y2="10" stroke="currentColor" stroke-width="1.2" />
        <line x1="10" y1="0" x2="0" y2="10" stroke="currentColor" stroke-width="1.2" />
      </svg>
    </button>
  </div>
</template>

<style scoped>
.window-controls {
  display: flex;
  align-items: stretch;
  height: var(--titlebar-height);
}

.win-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 46px;
  height: 100%;
  background: none;
  border: none;
  color: var(--color-text-muted);
  cursor: pointer;
  transition: background 0.1s, color 0.1s;
  padding: 0;
}

.win-btn:hover {
  background: var(--color-bg-elevated);
  color: var(--color-text-primary);
}

.win-btn.close:hover {
  background: var(--color-danger);
  color: #fff;
}
</style>
