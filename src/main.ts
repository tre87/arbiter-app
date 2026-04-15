import { createApp } from 'vue'
import { createPinia } from 'pinia'
import './style.css'

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
