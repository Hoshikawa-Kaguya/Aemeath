import { createApp } from 'vue'
import { createPinia } from 'pinia'
// 只注册用到的 Fluent 2 Web Components（基础控件优先用官方组件）
import '@fluentui/web-components/button/define.js'
import '@fluentui/web-components/dialog/define.js'
import '@fluentui/web-components/dialog-body/define.js'
import '@fluentui/web-components/progress-bar/define.js'
import '@vue-flow/core/dist/style.css'
import App from './App.vue'
import { i18n } from './i18n'
import { initTheme } from './theme/theme'
import './styles/base.css'
import './styles/tokens.css'

initTheme()

createApp(App).use(createPinia()).use(i18n).mount('#app')
