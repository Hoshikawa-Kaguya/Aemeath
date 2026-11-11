import {
  type BrandVariants,
  createDarkTheme,
  createLightTheme,
  type Theme,
  webDarkTheme,
  webLightTheme,
} from '@fluentui/tokens'
import { setTheme } from '@fluentui/web-components/theme/set-theme.js'
import type { Appearance } from '@/api/types'

const DARK_QUERY = '(prefers-color-scheme: dark)'
/** 色阶的编号：10、20 … 160，80 就是主题色本身 */
const BRAND_STEPS = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160] as const
const ANCHOR_STEP = 80
const LAST_STEP = 160
const LIGHTEST = 0.97
/** 主题色的亮度高于这个值时，按钮文字用黑色 */
const DARK_TEXT_LUMINANCE = 0.4
const LUMINANCE_R = 0.2126
const LUMINANCE_G = 0.7152
const LUMINANCE_B = 0.0722
const HEX_RADIX = 16
const CHANNEL_MAX = 255

type Rgb = [number, number, number]

function parseHex(hex: string): Rgb {
  return [1, 3, 5].map((start) => parseInt(hex.slice(start, start + 2), HEX_RADIX)) as Rgb
}

function toHex(rgb: Rgb): string {
  return `#${rgb.map((value) => Math.round(value).toString(HEX_RADIX).padStart(2, '0')).join('')}`
}

/** 相对亮度（sRGB），用来决定主题色上的文字用黑还是白 */
function luminance(rgb: Rgb): number {
  const [red, green, blue] = rgb.map((value) => {
    const channel = value / CHANNEL_MAX
    return channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4
  }) as Rgb
  return LUMINANCE_R * red + LUMINANCE_G * green + LUMINANCE_B * blue
}

/** 由一个颜色生成 Fluent 的 16 级色阶：80 级是它本身，往下混黑，往上混白 */
function brandRamp(hex: string): BrandVariants {
  const accent = parseHex(hex)
  const shade = (step: number): string => {
    if (step <= ANCHOR_STEP) {
      return toHex(accent.map((value) => (value * step) / ANCHOR_STEP) as Rgb)
    }
    const amount = (LIGHTEST * (step - ANCHOR_STEP)) / (LAST_STEP - ANCHOR_STEP)
    return toHex(accent.map((value) => value + (CHANNEL_MAX - value) * amount) as Rgb)
  }
  return Object.fromEntries(BRAND_STEPS.map((step) => [step, shade(step)])) as unknown as BrandVariants
}

/** 生成 Fluent 主题：没有自定义主题色时用官方默认主题，否则按主题色生成，并按亮度选主题色上的文字颜色 */
function buildTheme(isDark: boolean, accent: string | null): Theme {
  if (accent === null) {
    return isDark ? webDarkTheme : webLightTheme
  }
  const brand = brandRamp(accent)
  const theme = isDark ? createDarkTheme(brand) : createLightTheme(brand)
  return {
    ...theme,
    colorNeutralForegroundOnBrand: luminance(parseHex(accent)) > DARK_TEXT_LUMINANCE ? '#000000' : '#ffffff',
  }
}

let current: Pick<Appearance, 'themeMode' | 'accent'> = { themeMode: 'system', accent: null }
const query = window.matchMedia(DARK_QUERY)

/** 按当前设置和系统深浅色应用主题，并在根元素上标出深浅色，供少数需要区分的样式使用 */
function apply(): void {
  const isDark = current.themeMode === 'dark' || (current.themeMode === 'system' && query.matches)
  setTheme(buildTheme(isDark, current.accent))
  document.documentElement.dataset.theme = isDark ? 'dark' : 'light'
}

/**
 * 把 Fluent 2 设计令牌写成全局 CSS 变量（如 `--colorNeutralForeground1`），"跟随系统"时随系统切换立即更新。
 * 样式只引用这些变量，不写死颜色。
 */
export function initTheme(): void {
  apply()
  query.addEventListener('change', apply)
}

/** 应用深浅色和主题色。 */
export function applyAppearance(next: Pick<Appearance, 'themeMode' | 'accent'>): void {
  current = next
  apply()
}
