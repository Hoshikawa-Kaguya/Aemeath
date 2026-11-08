// 电平表的绘制（ADR-0013）：数据由后端经 Channel 约 30 Hz 推送，这里用一个 requestAnimationFrame 循环
// 统一绘制所有已注册的 <canvas>，完全绕过 Vue 的响应式更新。
//
// - 每次推送的是"自上次以来的峰值"，显示值取它与衰减后旧值中的较大者，两次推送之间按 DECAY_DB_PER_SECOND 衰减；
// - 电平稳定不动的表不重绘，静音、无信号时几乎没有开销；
// - dB 数字每秒只更新 READOUT_HZ 次，避免 64 个节点每帧改 DOM；
// - 颜色取自 CSS 变量（Fluent 令牌），主题切换时重新读取。

import { watchMeters, type MeterSample } from '@/api/backend'

/** 刻度下限：低于它视为无声 */
export const FLOOR_DB = -60
const DECAY_DB_PER_SECOND = 24
/** 色段分界：-18 dB 以下绿色，-6 dB 以上红色 */
const MID_DB = -18
const HIGH_DB = -6
const READOUT_HZ = 10
const MS_PER_SECOND = 1000
/** 立体声：两根条之间的间隔（CSS 像素） */
const BAR_GAP = 1

export function toDb(linear: number): number {
  return linear > 0 ? 20 * Math.log10(linear) : -Infinity
}

function fraction(db: number): number {
  return Math.max(0, Math.min(1, (db - FLOOR_DB) / -FLOOR_DB))
}

interface Level {
  left: number
  right: number
}

interface Meter {
  node: number
  canvas: HTMLCanvasElement
  readout: HTMLElement | null
  drawn: Level | null
  readoutText: string
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

  register(node: number, canvas: HTMLCanvasElement, readout: HTMLElement | null): () => void {
    const meter: Meter = { node, canvas, readout, drawn: null, readoutText: '' }
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
      this.draw(meter, palette, updateReadouts)
    }
    if (this.meters.size > 0) {
      this.frame = requestAnimationFrame((next) => this.tick(next))
    } else {
      this.lastFrame = 0
    }
  }

  private draw(meter: Meter, palette: Palette, updateReadout: boolean): void {
    const level = this.levels.get(meter.node) ?? { left: -Infinity, right: -Infinity }
    const shown = { left: fraction(level.left), right: fraction(level.right) }
    if (updateReadout && meter.readout !== null) {
      const peak = Math.max(level.left, level.right)
      const text = peak <= FLOOR_DB ? '−∞' : `${Math.round(peak)} dB`.replace('-', '−')
      if (text !== meter.readoutText) {
        meter.readoutText = text
        meter.readout.textContent = text
      }
    }
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
    const gap = BAR_GAP * ratio
    const barHeight = (height - gap) / 2
    context.clearRect(0, 0, width, height)
    const muted = this.muted.has(meter.node)
    for (const [index, value] of [shown.left, shown.right].entries()) {
      const y = index * (barHeight + gap)
      context.fillStyle = palette.track
      context.fillRect(0, y, width, barHeight)
      if (value <= 0) {
        continue
      }
      const end = value * width
      const midEnd = fraction(MID_DB) * width
      const highEnd = fraction(HIGH_DB) * width
      if (muted) {
        context.fillStyle = palette.muted
        context.fillRect(0, y, end, barHeight)
        continue
      }
      context.fillStyle = palette.low
      context.fillRect(0, y, Math.min(end, midEnd), barHeight)
      if (end > midEnd) {
        context.fillStyle = palette.mid
        context.fillRect(midEnd, y, Math.min(end, highEnd) - midEnd, barHeight)
      }
      if (end > highEnd) {
        context.fillStyle = palette.high
        context.fillRect(highEnd, y, end - highEnd, barHeight)
      }
    }
  }
}

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
