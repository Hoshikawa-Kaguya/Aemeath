// 悬停即可用滚轮调增益（连线标签、路由矩阵的格子），不必先点击选中。
//
// 防误触：区分一次滚动是从哪里开始的。
// - 指针先停在目标上，再开始滚动：这次滚动属于目标，调增益；
// - 指针是在滚动过程中移到目标上的（例如滚轮缩放工作台、滚动矩阵时扫过标签或格子）：这次滚动仍属于原来的区域，
//   照常缩放或滚动，不改增益；滚轮停下 GESTURE_GAP_MS 之后再滚，才算新的一次滚动。

/** 两次滚轮事件间隔超过这么久，就算新的一次滚动 */
const GESTURE_GAP_MS = 250
/** 滚动这么多像素调 1 dB：鼠标滚轮一格正好是 100 像素；触摸板一次滑动的事件很多、每个都很小，按距离累加 */
const PIXELS_PER_STEP = 100
/** 按行滚动的设备（少见）每行折算的像素 */
const PIXELS_PER_LINE = 33

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
  /** 累加滚动距离，返回这次该调的步数（向上为正，可能是 0） */
  steps(event: WheelEvent): number
}

/** 为一个目标（一个标签或一个格子）建立判断状态 */
export function createHoverWheel(): HoverWheel {
  /** 进入目标时，另一次滚动正在进行 */
  let inheritedGesture = false
  /** 还不够一步的滚动距离（像素，向上为正） */
  let accumulated = 0
  return {
    enter() {
      inheritedGesture = performance.now() - lastWheelAt < GESTURE_GAP_MS
      accumulated = 0
    },
    steps(event) {
      if (lastWheelAt - previousWheelAt >= GESTURE_GAP_MS) {
        accumulated = 0
      }
      const perUnit = event.deltaMode === WheelEvent.DOM_DELTA_LINE ? PIXELS_PER_LINE : 1
      accumulated -= event.deltaY * perUnit
      const steps = Math.trunc(accumulated / PIXELS_PER_STEP)
      accumulated -= steps * PIXELS_PER_STEP
      return steps
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
