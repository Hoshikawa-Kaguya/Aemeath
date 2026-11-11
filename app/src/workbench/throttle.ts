// 连续调节（推子、滑块）时限制发给后端的频率：每次编辑都会保存配置并重新下发拓扑，
// 拖动推子每秒可能产生上百个 input 事件，这里最多每 INTERVAL_MS 发一次，并保证最后一个值一定发出。

const INTERVAL_MS = 50

/** 包装 `send`：返回的函数可以随意频繁调用，实际发送受上述频率限制 */
export function throttled<T>(send: (value: T) => void): (value: T) => void {
  let timer = 0
  let pending: { value: T } | null = null
  let last = 0
  const flush = () => {
    timer = 0
    if (pending !== null) {
      last = performance.now()
      const { value } = pending
      pending = null
      send(value)
    }
  }
  return (value: T) => {
    pending = { value }
    if (timer !== 0) {
      return
    }
    const wait = Math.max(0, INTERVAL_MS - (performance.now() - last))
    timer = window.setTimeout(flush, wait)
  }
}
