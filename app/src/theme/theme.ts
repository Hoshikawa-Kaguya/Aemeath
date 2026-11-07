import { webDarkTheme, webLightTheme } from '@fluentui/tokens'
import { setTheme } from '@fluentui/web-components/theme/set-theme.js'

const DARK_QUERY = '(prefers-color-scheme: dark)'

/**
 * 跟随系统的深浅色：把 Fluent 2 设计令牌写成全局 CSS 变量（如 `--colorNeutralForeground1`），
 * 系统切换深浅色时立即更新。样式只引用这些变量，不写死颜色（ADR-0013）。
 */
export function followSystemTheme(): void {
  const query = window.matchMedia(DARK_QUERY)
  const apply = (): void => {
    const isDark = query.matches
    setTheme(isDark ? webDarkTheme : webLightTheme)
    document.documentElement.dataset.theme = isDark ? 'dark' : 'light'
  }
  apply()
  query.addEventListener('change', apply)
}
