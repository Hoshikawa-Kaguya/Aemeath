// 连线的曲线（沿用原型）：从输出端口水平引出、水平进入输入端口的三次贝塞尔曲线。
// 连线、拖线中的虚线、标签定位与拖动都用这一份。

/** 控制点离端点的最小水平距离：两端很近时曲线也保持舒展 */
const MIN_HANDLE = 60
const HANDLE_RATIO = 0.5
/** 标签位置的范围，与后端 LabelPosition 的限制一致（aemeath-core） */
export const LABEL_MIN = 0.1
export const LABEL_MAX = 0.9
/** 键盘移动标签的步长（Alt+← / Alt+→） */
export const LABEL_STEP = 0.05
/** 拖动标签时在曲线上搜索最近点的采样数 */
const SAMPLES = 80

export interface Point {
  x: number
  y: number
}

export interface Curve {
  path: string
  start: Point
  control1: Point
  control2: Point
  end: Point
}

export function routeCurve(start: Point, end: Point): Curve {
  const handle = Math.max(MIN_HANDLE, Math.abs(end.x - start.x) * HANDLE_RATIO)
  const control1 = { x: start.x + handle, y: start.y }
  const control2 = { x: end.x - handle, y: end.y }
  return {
    path: `M${start.x},${start.y} C${control1.x},${control1.y} ${control2.x},${control2.y} ${end.x},${end.y}`,
    start,
    control1,
    control2,
    end,
  }
}

/** 曲线上参数 t（0 起点、1 终点）处的点 */
export function pointAt(curve: Curve, t: number): Point {
  const u = 1 - t
  const a = u * u * u
  const b = 3 * u * u * t
  const c = 3 * u * t * t
  const d = t * t * t
  return {
    x: a * curve.start.x + b * curve.control1.x + c * curve.control2.x + d * curve.end.x,
    y: a * curve.start.y + b * curve.control1.y + c * curve.control2.y + d * curve.end.y,
  }
}

/** 曲线上离 target 最近的点的参数，限制在标签位置的范围内 */
export function nearestT(curve: Curve, target: Point): number {
  let best = 0.5
  let bestDistance = Infinity
  for (let index = 0; index <= SAMPLES; index++) {
    const t = LABEL_MIN + ((LABEL_MAX - LABEL_MIN) * index) / SAMPLES
    const point = pointAt(curve, t)
    const distance = (point.x - target.x) ** 2 + (point.y - target.y) ** 2
    if (distance < bestDistance) {
      bestDistance = distance
      best = t
    }
  }
  return best
}

export function clampLabel(t: number): number {
  return Math.min(LABEL_MAX, Math.max(LABEL_MIN, t))
}
