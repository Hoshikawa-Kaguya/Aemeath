// 电平表的绘制：数据由后端经 Channel 约 30 Hz 推送，这里用一个 requestAnimationFrame 循环
// 统一绘制所有已注册的 <canvas>，完全绕过 Vue 的响应式更新。
//
// - 每次推送的是"自上次以来的峰值"，显示值取它与衰减后旧值中的较大者，两次推送之间按 DECAY_DB_PER_SECOND 衰减；
// - 电平稳定不动的表不重绘，静音、无信号时几乎没有开销；
// - dB 数字每秒只更新 READOUT_HZ 次，避免 64 个节点每帧改 DOM；控制台通道条的读数更慢（STRIP_READOUT_MS），显示这段时间内的最高峰值；
// - 颜色取自 CSS 变量（Fluent 令牌），主题切换时重新读取。

import { watchMeters, type MeterSample } from '@/api/backend'

/** 刻度下限：低于它视为无声 */
export const FLOOR_DB = -60
const DECAY_DB_PER_SECOND = 24
/** 色段分界：-18 dB 以下绿色，-6 dB 以上红色 */
const MID_DB = -18
const HIGH_DB = -6
const READOUT_HZ = 10
const STRIP_READOUT_MS = 250
const MS_PER_SECOND = 1000
const SILENT_TEXT = '−∞'
/** 立体声：两根条之间的间隔（CSS 像素） */
const BAR_GAP = 1

/** 线性峰值换算成 dBFS；0 为 −∞ */
export function toDb(linear: number): number {
  return linear > 0 ? 20 * Math.log10(linear) : -Infinity
}

/** dBFS 在电平条上的位置（0 是最底端的 FLOOR_DB，1 是 0 dBFS） */
function fraction(db: number): number {
  return Math.max(0, Math.min(1, (db - FLOOR_DB) / -FLOOR_DB))
}

interface Level {
  left: number
  right: number
}

/** 横向（工作台节点：两根条上下排列，从左往右长）或竖向（控制台通道条：两根条左右排列，从下往上长） */
export type MeterOrientation = 'horizontal' | 'vertical'

interface Meter {
  node: number
  canvas: HTMLCanvasElement
  readout: HTMLElement | null
  orientation: MeterOrientation
  drawn: Level | null
  readoutText: string
  /** 竖向读数：当前时间窗内的最高峰值（dB）与时间窗的起点 */
  windowPeak: number
  windowStart: number
}

interface Palette {
  track: string
  low: string
  mid: string
  high: string
  muted: string
}

class MeterRenderer {
  /** 每个节点的显示电平（dB） */
  private readonly levels = new Map<number, Level>()
  private readonly meters = new Set<Meter>()
  private readonly muted = new Set<number>()
  private palette: Palette | null = null
  private frame = 0
  private lastFrame = 0
  private lastReadout = 0
  private subscribed = false

  register(
    node: number,
    canvas: HTMLCanvasElement,
    readout: HTMLElement | null,
    orientation: MeterOrientation = 'horizontal',
  ): () => void {
    const meter: Meter = {
      node,
      canvas,
      readout,
      orientation,
      drawn: null,
      readoutText: '',
      windowPeak: -Infinity,
      windowStart: 0,
    }
    this.meters.add(meter)
    this.ensureRunning()
    return () => {
      this.meters.delete(meter)
    }
  }

  setMuted(node: number, muted: boolean): void {
    if (muted) {
      this.muted.add(node)
    } else {
      this.muted.delete(node)
    }
  }

  /** 主题变了：下一帧重新读取颜色并重绘全部 */
  invalidatePalette(): void {
    this.palette = null
    for (const meter of this.meters) {
      meter.drawn = null
    }
  }

  private ensureRunning(): void {
    if (!this.subscribed) {
      this.subscribed = true
      watchMeters((samples) => this.feed(samples)).catch(() => {
        // 不在 Tauri 里运行（例如浏览器预览）时没有后端，电平保持为空
      })
    }
    if (this.frame === 0) {
      this.frame = requestAnimationFrame((time) => this.tick(time))
    }
  }

  private feed(samples: MeterSample[]): void {
    // 没有电平表时不累加：否则回到工作台时每个表都从这段时间的最高峰开始回落，像刚爆过音
    if (this.meters.size === 0) {
      this.levels.clear()
      return
    }
    for (const sample of samples) {
      const current = this.levels.get(sample.node) ?? { left: -Infinity, right: -Infinity }
      current.left = Math.max(current.left, toDb(sample.left))
      current.right = Math.max(current.right, toDb(sample.right))
      this.levels.set(sample.node, current)
    }
  }

  private tick(time: number): void {
    this.frame = 0
    const elapsed = this.lastFrame === 0 ? 0 : (time - this.lastFrame) / MS_PER_SECOND
    this.lastFrame = time
    const decay = DECAY_DB_PER_SECOND * elapsed
    for (const level of this.levels.values()) {
      level.left = Math.max(level.left - decay, -Infinity)
      level.right = Math.max(level.right - decay, -Infinity)
    }
    const updateReadouts = time - this.lastReadout >= MS_PER_SECOND / READOUT_HZ
    if (updateReadouts) {
      this.lastReadout = time
    }
    const palette = (this.palette ??= readPalette())
    for (const meter of this.meters) {
      this.updateReadout(meter, time, updateReadouts)
      this.draw(meter, palette)
    }
    if (this.meters.size > 0) {
      this.frame = requestAnimationFrame((next) => this.tick(next))
    } else {
      this.lastFrame = 0
    }
  }

  private updateReadout(meter: Meter, time: number, updateHorizontal: boolean): void {
    if (meter.readout === null) {
      return
    }
    const level = this.levels.get(meter.node)
    const peak = level === undefined ? -Infinity : Math.max(level.left, level.right)
    let text: string
    if (meter.orientation === 'vertical') {
      meter.windowPeak = Math.max(meter.windowPeak, peak)
      if (time - meter.windowStart < STRIP_READOUT_MS) {
        return
      }
      text =
        meter.windowPeak <= FLOOR_DB || this.muted.has(meter.node)
          ? SILENT_TEXT
          : meter.windowPeak.toFixed(1).replace('-', '−')
      meter.windowPeak = -Infinity
      meter.windowStart = time
    } else {
      if (!updateHorizontal) {
        return
      }
      text = peak <= FLOOR_DB ? SILENT_TEXT : `${Math.round(peak)} dB`.replace('-', '−')
    }
    if (text !== meter.readoutText) {
      meter.readoutText = text
      meter.readout.textContent = text
    }
  }

  private draw(meter: Meter, palette: Palette): void {
    const level = this.levels.get(meter.node) ?? { left: -Infinity, right: -Infinity }
    const shown = { left: fraction(level.left), right: fraction(level.right) }
    if (meter.drawn !== null && meter.drawn.left === shown.left && meter.drawn.right === shown.right) {
      return
    }
    meter.drawn = shown
    const canvas = meter.canvas
    const ratio = window.devicePixelRatio || 1
    const width = Math.round(canvas.clientWidth * ratio)
    const height = Math.round(canvas.clientHeight * ratio)
    if (width === 0 || height === 0) {
      return
    }
    if (canvas.width !== width || canvas.height !== height) {
      canvas.width = width
      canvas.height = height
    }
    const context = canvas.getContext('2d')
    if (context === null) {
      return
    }
    // 沿电平增长方向的长度（length）与两根条并排方向的宽度（breadth）；竖向时从底部往上画
    const vertical = meter.orientation === 'vertical'
    const length = vertical ? height : width
    const breadth = vertical ? width : height
    const gap = BAR_GAP * ratio
    const barBreadth = (breadth - gap) / 2
    /** 在"沿电平方向 [start, end)、第 index 根条"的位置填色 */
    const fill = (index: number, start: number, end: number) => {
      const across = index * (barBreadth + gap)
      if (vertical) {
        context.fillRect(across, length - end, barBreadth, end - start)
      } else {
        context.fillRect(start, across, end - start, barBreadth)
      }
    }
    context.clearRect(0, 0, width, height)
    const muted = this.muted.has(meter.node)
    const midEnd = fraction(MID_DB) * length
    const highEnd = fraction(HIGH_DB) * length
    for (const [index, value] of [shown.left, shown.right].entries()) {
      context.fillStyle = palette.track
      fill(index, 0, length)
      if (value <= 0) {
        continue
      }
      const end = value * length
      if (muted) {
        context.fillStyle = palette.muted
        fill(index, 0, end)
        continue
      }
      context.fillStyle = palette.low
      fill(index, 0, Math.min(end, midEnd))
      if (end > midEnd) {
        context.fillStyle = palette.mid
        fill(index, midEnd, Math.min(end, highEnd))
      }
      if (end > highEnd) {
        context.fillStyle = palette.high
        fill(index, highEnd, end)
      }
    }
  }
}

/** 从当前主题的 CSS 变量读出电平表的颜色；canvas 不能直接引用 CSS 变量 */
function readPalette(): Palette {
  const style = getComputedStyle(document.documentElement)
  const read = (name: string) => style.getPropertyValue(name).trim()
  return {
    track: read('--meter-track'),
    low: read('--meter-low'),
    mid: read('--meter-mid'),
    high: read('--meter-high'),
    muted: read('--colorNeutralForegroundDisabled'),
  }
}

export const meterRenderer = new MeterRenderer()

// 深浅主题切换时（theme.ts 改写 data-theme）重新读取颜色
new MutationObserver(() => meterRenderer.invalidatePalette()).observe(document.documentElement, {
  attributes: true,
  attributeFilter: ['data-theme'],
})
