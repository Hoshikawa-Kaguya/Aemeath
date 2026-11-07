import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// 端口与 src-tauri/tauri.conf.json 的 devUrl 一致；端口被占用时直接失败，而不是换端口让 Tauri 连错
const DEV_PORT = 1420

export default defineConfig({
  plugins: [vue()],
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
