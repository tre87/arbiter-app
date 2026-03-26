<script setup lang="ts">
import { onMounted, onBeforeUnmount } from 'vue'
import { usePaneStore } from './stores/pane'
import SplitView from './components/SplitView.vue'
import StatsBar from './components/StatsBar.vue'
import MdiIcon from './components/MdiIcon.vue'
import { mdiCogOutline } from '@mdi/js'
import logoUrl from './assets/logo.svg'

const store = usePaneStore()

function handleKeyDown(e: KeyboardEvent) {
  if (!e.ctrlKey) return

  // Ctrl+Shift+T → split horizontal (stacked top/bottom)
  if (e.shiftKey && e.key === 'T') {
    e.preventDefault()
    e.stopPropagation()
    store.splitFocused('horizontal')
    return
  }

  // Ctrl+T → split vertical (side by side)
  if (!e.shiftKey && e.key === 't') {
    e.preventDefault()
    e.stopPropagation()
    store.splitFocused('vertical')
    return
  }

  // Ctrl+W → close focused pane
  if (!e.shiftKey && e.key === 'w') {
    e.preventDefault()
    e.stopPropagation()
    store.closeFocused()
  }
}

onMounted(() => window.addEventListener('keydown', handleKeyDown, { capture: true }))
onBeforeUnmount(() => window.removeEventListener('keydown', handleKeyDown, { capture: true }))
</script>

<template>
  <div class="app">
    <div class="titlebar">
      <div class="titlebar-brand">
        <img :src="logoUrl" class="titlebar-logo" alt="Arbiter" />
        <span class="titlebar-title">Arbiter</span>
      </div>
      <div class="titlebar-center">
        <StatsBar />
      </div>
      <div class="titlebar-actions">
        <button class="settings-btn" title="Settings" @click="() => {}">
          <MdiIcon :path="mdiCogOutline" :size="16" />
        </button>
      </div>
    </div>
    <div class="workspace">
      <SplitView :node="store.root" />
    </div>
  </div>
</template>

<style scoped>
.app {
  display: flex;
  flex-direction: column;
  height: 100vh;
  width: 100vw;
}

.titlebar {
  height: 46px;
  background: var(--color-bg-subtle);
  border-bottom: 1px solid var(--color-card-border);
  display: grid;
  grid-template-columns: auto 1fr auto;
  align-items: center;
  padding: 0 12px 0 6px;
  user-select: none;
  -webkit-app-region: drag;
  flex-shrink: 0;
}

.titlebar-center {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 10px;
  -webkit-app-region: no-drag;
}

.titlebar-actions {
  display: flex;
  align-items: center;
  gap: 6px;
  -webkit-app-region: no-drag;
}

.settings-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  background: none;
  border: 1px solid var(--color-card-border);
  border-radius: 4px;
  color: var(--color-text-muted);
  cursor: pointer;
  padding: 4px;
  line-height: 1;
  transition: color 0.15s, border-color 0.15s, background 0.15s;
}

.settings-btn:hover {
  color: var(--color-text-primary);
  border-color: var(--color-text-muted);
  background: var(--color-bg-elevated);
}

.titlebar-brand {
  display: flex;
  align-items: center;
  gap: 5px;
}

.titlebar-logo {
  width: 28px;
  height: 28px;
  flex-shrink: 0;
}

.titlebar-title {
  font-family: 'Chakra Petch', sans-serif;
  font-weight: 700;
  font-size: 13px;
  letter-spacing: 0.18em;
  text-transform: uppercase;
  background: linear-gradient(
    90deg,
    var(--azure-baby)    0%,
    var(--azure)         25%,
    var(--azure-deep)    50%,
    var(--azure-tropical) 75%,
    var(--azure-baby)    100%
  );
  background-size: 250% auto;
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
  animation: title-shimmer 6s ease-in-out infinite alternate;
}

@keyframes title-shimmer {
  from { background-position: 0% center; }
  to   { background-position: 100% center; }
}

.workspace {
  flex: 1;
  overflow: hidden;
  background: var(--color-bg);
}
</style>
