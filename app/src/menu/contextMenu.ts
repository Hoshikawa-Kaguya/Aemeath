// 自己的右键菜单（M2 计划 2.2）：WebView 自带的菜单（刷新、检查等）全局屏蔽，由各处按对象提供菜单项。
// 全应用只有一个菜单实例（ContextMenu.vue 放在 App 中），任何组件调用 openMenu 打开。

import { shallowRef } from 'vue'

export interface MenuAction {
  kind: 'action'
  label: string
  run: () => void
  disabled?: boolean
  /** 删除这类破坏性操作，用警示色显示 */
  danger?: boolean
}

export interface MenuSeparator {
  kind: 'separator'
}

export type MenuItem = MenuAction | MenuSeparator

export interface OpenMenu {
  items: MenuItem[]
  x: number
  y: number
  /** 关闭后把焦点还给它（键盘打开时是触发元素） */
  returnFocus: HTMLElement | null
}

export const SEPARATOR: MenuSeparator = { kind: 'separator' }

export const openedMenu = shallowRef<OpenMenu | null>(null)

/** 在指针位置打开菜单；用菜单键或 Shift+F10 打开时，放在触发元素的左下角。 */
export function openMenu(event: MouseEvent | KeyboardEvent, items: MenuItem[]): void {
  event.preventDefault()
  event.stopPropagation()
  const target = event.currentTarget instanceof HTMLElement ? event.currentTarget : null
  let x = 0
  let y = 0
  if (event instanceof MouseEvent && (event.clientX !== 0 || event.clientY !== 0)) {
    x = event.clientX
    y = event.clientY
  } else if (target !== null) {
    const rect = target.getBoundingClientRect()
    x = rect.left
    y = rect.bottom
  }
  openedMenu.value = { items, x, y, returnFocus: document.activeElement as HTMLElement | null }
}

/** 在指定的窗口坐标打开菜单（例如键盘打开时，放在选中对象旁边）。 */
export function openMenuAt(point: { x: number; y: number }, items: MenuItem[]): void {
  openedMenu.value = { items, x: point.x, y: point.y, returnFocus: document.activeElement as HTMLElement | null }
}

export function closeMenu(): void {
  const menu = openedMenu.value
  openedMenu.value = null
  menu?.returnFocus?.focus?.()
}

/** 键盘上的"打开菜单"：菜单键，或 Shift+F10 */
export function isMenuKey(event: KeyboardEvent): boolean {
  return event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10')
}
