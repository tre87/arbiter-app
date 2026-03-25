import { createApp } from 'vue'
import { createPinia } from 'pinia'
import App from './App.vue'
import '@fontsource/chakra-petch/700.css'
import './style.css'
import '@xterm/xterm/css/xterm.css'

createApp(App).use(createPinia()).mount('#app')
