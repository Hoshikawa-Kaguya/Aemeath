import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// 端口与 src-tauri/tauri.conf.json 的 devUrl 一致；端口被占用时直接失败
const DEV_PORT = 21364

export default defineConfig({
  plugins: [
    vue({
      template: {
        compilerOptions: {
          // Fluent 2 的 Web Components（fluent-button 等）由浏览器渲染，Vue 不当作组件解析
          isCustomElement: (tag) => tag.startsWith('fluent-'),
        },
      },
    }),
  ],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  // 不清屏，保留 Rust 编译输出
  clearScreen: false,
  server: {
    port: DEV_PORT,
    strictPort: true,
    watch: {
      ignored: ['**/src-tauri/**', '**/crates/**'],
    },
  },
  build: {
    // 只运行在 Windows 11 自带的 WebView2（Chromium）里
    target: 'chrome120',
  },
})
