import { onMounted, onBeforeUnmount } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { usePaneStore } from '../stores/pane'
import { useFilesSettingsStore } from '../stores/filesSettings'
import { useConfirm } from './useConfirm'
import { computeLeafRects, findNeighbor, findResizableSplit, type Direction } from '../utils/spatial'
import { dirnameOf, pickAndAttach } from '../utils/attachFiles'

const arrowToDirection: Record<string, Direction> = {
  ArrowLeft: 'left',
  ArrowRight: 'right',
  ArrowUp: 'up',
  ArrowDown: 'down',
}

export function useKeyboardShortcuts(toggleOverview: () => void) {
  const store = usePaneStore()
  const filesStore = useFilesSettingsStore()
  const { confirm: confirmDialog } = useConfirm()

  async function attachFromScreenshots() {
    try {
      const dir = await filesStore.resolveScreenshotDir()
      await pickAndAttach(dir)
    } catch (e) {
      console.error('Arbiter: attachFromScreenshots failed:', e)
    }
  }

  async function attachFromDocs() {
    try {
      const dir = await filesStore.resolveDocsDir()
      const picked = await pickAndAttach(dir)
      if (picked.length) filesStore.setLastDocsFolder(dirnameOf(picked[0]))
    } catch (e) {
      console.error('Arbiter: attachFromDocs failed:', e)
    }
  }

  async function confirmCloseWorkspace(index: number) {
    const ws = store.workspaces[index]
    if (!ws) return
    const ok = await confirmDialog({
      title: `Close workspace "${ws.name}"?`,
      message: ws.type === 'project'
        ? 'All terminals in this project workspace will be closed.'
        : 'All terminals in this workspace will be closed.',
      confirmText: 'Close',
      danger: true,
    })
    if (ok) store.removeWorkspace(index)
  }

  function handleKeyDown(e: KeyboardEvent) {
    // Alt+Shift+Arrow → resize focused pane
    if (e.altKey && e.shiftKey && !e.ctrlKey) {
      const direction = arrowToDirection[e.code]
      if (!direction) return
      e.preventDefault()
      e.stopPropagation()
      const result = findResizableSplit(store.root, store.focusedId, direction)
      if (result) store.adjustSplitSize(result.splitId, result.delta * 5)
      return
    }

    // Ctrl+Shift+T → new workspace tab
    if (e.ctrlKey && e.shiftKey && e.code === 'KeyT') {
      e.preventDefault()
      e.stopPropagation()
      store.addWorkspace()
      return
    }

    // Ctrl+Shift+I → open WebView2 DevTools (release builds include the
    // `devtools` Tauri feature). Handled before the generic Ctrl+Shift gate.
    if (e.ctrlKey && e.shiftKey && e.code === 'KeyI') {
      e.preventDefault()
      e.stopPropagation()
      invoke('open_devtools').catch(err => console.error('Arbiter: open_devtools failed:', err))
      return
    }

    // Ctrl+Tab / Ctrl+Shift+Tab → next/prev workspace
    if (e.ctrlKey && e.code === 'Tab') {
      e.preventDefault()
      e.stopPropagation()
      const count = store.workspaces.length
      if (count <= 1) return
      const delta = e.shiftKey ? -1 : 1
      const next = (store.activeWorkspaceIndex + delta + count) % count
      store.switchWorkspace(next)
      return
    }

    // Ctrl+1..9 → switch to workspace by number
    if (e.ctrlKey && !e.shiftKey && !e.altKey) {
      const digitMatch = e.code.match(/^Digit([1-9])$/)
      if (digitMatch) {
        const idx = parseInt(digitMatch[1], 10) - 1
        if (idx < store.workspaces.length) {
          e.preventDefault()
          e.stopPropagation()
          store.switchWorkspace(idx)
          return
        }
      }
    }

    if (!e.ctrlKey || !e.shiftKey) return

    // Ctrl+Shift+Arrow → navigate panes
    const direction = arrowToDirection[e.code]
    if (direction) {
      e.preventDefault()
      e.stopPropagation()
      const rects = computeLeafRects(store.root)
      const neighbor = findNeighbor(rects, store.focusedId, direction)
      if (neighbor) store.setFocus(neighbor)
      return
    }

    // Ctrl+Shift+O → workspace overview
    if (e.code === 'KeyO') {
      e.preventDefault()
      e.stopPropagation()
      toggleOverview()
      return
    }

    // Ctrl+Shift+E → equalize all split sizes (even grid)
    if (e.code === 'KeyE') {
      e.preventDefault()
      e.stopPropagation()
      store.equalizeSplits()
      return
    }

    // Ctrl+Shift+S → attach files from screenshot folder
    if (e.code === 'KeyS') {
      e.preventDefault()
      e.stopPropagation()
      attachFromScreenshots()
      return
    }

    // Ctrl+Shift+A → attach files from last-used folder (Documents on first run)
    if (e.code === 'KeyA') {
      e.preventDefault()
      e.stopPropagation()
      attachFromDocs()
      return
    }

    // Ctrl+Shift+R → split right (vertical, side by side)
    if (e.code === 'KeyR') {
      e.preventDefault()
      e.stopPropagation()
      store.splitFocused('vertical')
      return
    }

    // Ctrl+Shift+D → split down (horizontal, stacked)
    if (e.code === 'KeyD') {
      e.preventDefault()
      e.stopPropagation()
      store.splitFocused('horizontal')
      return
    }

    // Ctrl+Shift+W → close focused pane, or close workspace if last pane
    if (e.code === 'KeyW') {
      e.preventDefault()
      e.stopPropagation()
      const currentWs = store.workspaces[store.activeWorkspaceIndex]
      if (currentWs.type === 'terminal' && store.root.type === 'terminal' && store.workspaces.length > 1) {
        confirmCloseWorkspace(store.activeWorkspaceIndex)
      } else {
        store.closeFocused()
      }
    }
  }

  onMounted(() => window.addEventListener('keydown', handleKeyDown, { capture: true }))
  onBeforeUnmount(() => window.removeEventListener('keydown', handleKeyDown, { capture: true }))
}
