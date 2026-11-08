// 悬停即可用滚轮调增益（连线标签、路由矩阵的格子；维护者 2026-11-08 要求不必先点击选中）。
//
// 防误触：区分一次滚动是从哪里开始的。
// - 指针先停在目标上，再开始滚动：这次滚动属于目标，调增益；
// - 指针是在滚动过程中移到目标上的（例如滚动平移工作台、滚动矩阵时扫过标签或格子）：这次滚动仍属于原来的区域，
//   照常平移或滚动，不改增益；滚轮停下 GESTURE_GAP_MS 之后再滚，才算新的一次滚动。

/** 两次滚轮事件间隔超过这么久，就算新的一次滚动 */
const GESTURE_GAP_MS = 250

/** 最近一次滚轮事件的时刻，以及它之前那一次的时刻（在捕获阶段记录，先于目标自己的处理函数） */
let lastWheelAt = Number.NEGATIVE_INFINITY
let previousWheelAt = Number.NEGATIVE_INFINITY

window.addEventListener(
  'wheel',
  () => {
    previousWheelAt = lastWheelAt
    lastWheelAt = performance.now()
  },
  { capture: true, passive: true },
)

export interface HoverWheel {
  /** 指针进入目标时调用 */
  enter(): void
  /** 目标上的滚轮事件是否应该用来调增益 */
  accepts(): boolean
}

export function createHoverWheel(): HoverWheel {
  /** 进入目标时，另一次滚动正在进行 */
  let inheritedGesture = false
  return {
    enter() {
      inheritedGesture = performance.now() - lastWheelAt < GESTURE_GAP_MS
    },
    accepts() {
      if (inheritedGesture) {
        if (lastWheelAt - previousWheelAt < GESTURE_GAP_MS) {
          return false
        }
        inheritedGesture = false
      }
      return true
    },
  }
}
