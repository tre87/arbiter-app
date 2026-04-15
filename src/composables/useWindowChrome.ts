import { onMounted, onBeforeUnmount } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import { usePaneStore } from '../stores/pane'

/** Titlebar drag handlers. Explicit drag handling is more reliable than
 *  data-tauri-drag-region under custom chrome. */
export function useTitlebarDrag() {
  function onTitlebarMouseDown(e: MouseEvent) {
    if (e.button !== 0) return
    const target = e.target as HTMLElement | null
    if (!target) return
    if (target.closest('button, input, textarea, select, a, .tab')) return
    e.preventDefault()
    getCurrentWindow().startDragging()
  }

  async function onTitlebarDblClick(e: MouseEvent) {
    const target = e.target as HTMLElement | null
    if (!target) return
    if (target.closest('button, input, textarea, select, a, .tab')) return
    await getCurrentWindow().toggleMaximize()
  }

  return { onTitlebarMouseDown, onTitlebarDblClick }
}

/** Global context menu suppression + drag-and-drop → terminal paste bridge. */
export function useWindowChrome() {
  const store = usePaneStore()
  let unlistenDragDrop: (() => void) | null = null

  function handleContextMenu(e: MouseEvent) {
    e.preventDefault()
  }

  async function setupDragDrop() {
    const webview = getCurrentWebview()
    unlistenDragDrop = await webview.onDragDropEvent((event) => {
      if (event.payload.type !== 'drop') return
      const paths = (event.payload as any).paths as string[]
      if (!paths?.length) return

      // Position is in physical pixels — convert to logical for elementFromPoint
      const dpr = window.devicePixelRatio || 1
      const x = event.payload.position.x / dpr
      const y = event.payload.position.y / dpr

      const el = document.elementFromPoint(x, y)
      const paneEl = el?.closest('.terminal-pane') as HTMLElement | null
      if (!paneEl) return

      const paneId = paneEl.dataset.paneId
      if (!paneId) return

      const ptySessionId = store.getPtySession(paneId)
      if (!ptySessionId) return

      const quoted = paths.map(p => p.includes(' ') ? `"${p}"` : p)
      invoke('write_to_session', { sessionId: ptySessionId, data: quoted.join(' ') })
      store.setFocus(paneId)
    })
  }

  onMounted(() => {
    window.addEventListener('contextmenu', handleContextMenu)
    setupDragDrop()
  })
  onBeforeUnmount(() => {
    window.removeEventListener('contextmenu', handleContextMenu)
    unlistenDragDrop?.()
  })
}
