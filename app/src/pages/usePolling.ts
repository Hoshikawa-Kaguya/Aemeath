// 页面打开期间定时刷新（应用页、工具页：会话和默认设备随时会被其他程序改变，系统不一定通知）。

import { onBeforeUnmount, onMounted } from 'vue'

/** 挂载时立即调用一次，之后每 `intervalMs` 调用一次；上一次还没结束时跳过 */
export function usePolling(refresh: () => Promise<void>, intervalMs: number): void {
  let timer = 0
  let running = false
  const tick = async (): Promise<void> => {
    if (running) {
      return
    }
    running = true
    try {
      await refresh()
    } finally {
      running = false
    }
  }
  onMounted(() => {
    void tick()
    timer = window.setInterval(() => void tick(), intervalMs)
  })
  onBeforeUnmount(() => window.clearInterval(timer))
}
