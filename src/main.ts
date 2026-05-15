import { createApp } from 'vue'
import { createPinia } from 'pinia'
import './style.css'

// Set platform class synchronously so titlebar padding (e.g. 96px for macOS
// traffic lights) is correct on first paint — otherwise the titlebar visibly
// jumps once App.vue's async import resolves and adds the class itself.
const platform = navigator.platform
const platformClass = platform.startsWith('Mac') ? 'is-macos'
  : platform.startsWith('Win') ? 'is-windows'
  : 'is-linux'
document.body.classList.add(platformClass)

const label = (window as any).__TAURI_INTERNALS__?.metadata?.currentWindow?.label ?? 'main'

if (label === 'overview') {
  import('./OverviewApp.vue').then(({ default: OverviewApp }) => {
    createApp(OverviewApp).mount('#app')
  })
} else {
  import('@fontsource/chakra-petch/700.css')
  import('@xterm/xterm/css/xterm.css')
  import('./App.vue').then(({ default: App }) => {
    createApp(App).use(createPinia()).mount('#app')
  })
}
