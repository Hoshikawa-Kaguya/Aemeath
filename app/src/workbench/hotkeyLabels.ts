// 热键的显示文字（热键页、节点设置、连线的"热键…"对话框共用）。

import type { HotkeyTargetView, HotkeyView, ModifiersView, NodeView } from '@/api/types'
import { i18n } from '@/i18n'
import { nodeCode } from '@/stores/nodeLabels'

/** 一次录入的结果 */
export type CaptureResult = { type: 'captured'; key: HotkeyView } | { type: 'cancelled' } | { type: 'cleared' }

const VK_F1 = 0x70
const VK_F24 = 0x87
const VK_NUMPAD0 = 0x60
const VK_NUMPAD9 = 0x69
const VK_0 = 0x30
const VK_9 = 0x39
const VK_A = 0x41
const VK_Z = 0x5a
const HEX = 16
const KEYCODE_IME = 229
const BUTTON_X1 = 3
const BUTTON_X2 = 4
const MODIFIER_KEYS = ['Control', 'Alt', 'Shift', 'Meta']

const NAMED_KEYS: Record<number, string> = {
  0x09: 'Tab',
  0x0d: 'Enter',
  0x13: 'Pause',
  0x14: 'Caps Lock',
  0x20: 'Space',
  0x21: 'Page Up',
  0x22: 'Page Down',
  0x23: 'End',
  0x24: 'Home',
  0x25: '←',
  0x26: '↑',
  0x27: '→',
  0x28: '↓',
  0x2c: 'Print Screen',
  0x2d: 'Insert',
  0x2e: 'Delete',
  0x6a: 'Num *',
  0x6b: 'Num +',
  0x6d: 'Num -',
  0x6e: 'Num .',
  0x6f: 'Num /',
  0x90: 'Num Lock',
  0x91: 'Scroll Lock',
  0xba: ';',
  0xbb: '=',
  0xbc: ',',
  0xbd: '-',
  0xbe: '.',
  0xbf: '/',
  0xc0: '`',
  0xdb: '[',
  0xdc: '\\',
  0xdd: ']',
  0xde: "'",
}

function keyName(vk: number): string {
  if (vk >= VK_F1 && vk <= VK_F24) {
    return `F${vk - VK_F1 + 1}`
  }
  if (vk >= VK_NUMPAD0 && vk <= VK_NUMPAD9) {
    return `Num ${vk - VK_NUMPAD0}`
  }
  if ((vk >= VK_0 && vk <= VK_9) || (vk >= VK_A && vk <= VK_Z)) {
    return String.fromCharCode(vk)
  }
  return NAMED_KEYS[vk] ?? `VK 0x${vk.toString(HEX).toUpperCase()}`
}

/** 热键的显示文字，例如"Ctrl + Shift + M"；修饰键按 Windows 的习惯顺序排列 */
export function formatHotkey(hotkey: HotkeyView): string {
  const { t } = i18n.global
  const parts: string[] = []
  if (hotkey.modifiers.ctrl) {
    parts.push('Ctrl')
  }
  if (hotkey.modifiers.alt) {
    parts.push('Alt')
  }
  if (hotkey.modifiers.shift) {
    parts.push('Shift')
  }
  if (hotkey.modifiers.win) {
    parts.push('Win')
  }
  switch (hotkey.key.type) {
    case 'keyboard':
      parts.push(keyName(hotkey.key.vk))
      break
    case 'mouseX1':
      parts.push(t('hotkey.mouseX1'))
      break
    case 'mouseX2':
      parts.push(t('hotkey.mouseX2'))
      break
  }
  return parts.join(' + ')
}

function modifiersOf(event: KeyboardEvent | PointerEvent): ModifiersView {
  return { ctrl: event.ctrlKey, alt: event.altKey, shift: event.shiftKey, win: event.metaKey }
}

/** Windows 上 keyCode 就是虚拟键码；输入法组字时它是 229，字母和数字改从 code 推出来 */
function virtualKey(event: KeyboardEvent): number | null {
  if (event.keyCode !== KEYCODE_IME) {
    return event.keyCode
  }
  const match = /^(?:Key|Digit)(.)$/.exec(event.code)
  return match === null ? null : (match[1] ?? '').charCodeAt(0)
}

/** 录入时收到的按键；单独的修饰键还不算录入完成（返回 null） */
export function captureFromKey(event: KeyboardEvent): CaptureResult | null {
  if (MODIFIER_KEYS.includes(event.key)) {
    return null
  }
  if (event.key === 'Escape') {
    return { type: 'cancelled' }
  }
  if (event.key === 'Backspace') {
    return { type: 'cleared' }
  }
  const vk = virtualKey(event)
  return vk === null
    ? null
    : { type: 'captured', key: { modifiers: modifiersOf(event), key: { type: 'keyboard', vk } } }
}

/** 鼠标侧键录成热键，其他按键表示点了别处，取消录入 */
export function captureFromPointer(event: PointerEvent): CaptureResult {
  const key = event.button === BUTTON_X1 ? 'mouseX1' : event.button === BUTTON_X2 ? 'mouseX2' : null
  return key === null
    ? { type: 'cancelled' }
    : { type: 'captured', key: { modifiers: modifiersOf(event), key: { type: key } } }
}

/** 热键作用的对象，例如"A1 Rear Mic · 静音""IN1 → OUT2 · 开关" */
export function describeTarget(target: HotkeyTargetView, nodes: readonly NodeView[]): string {
  const { t } = i18n.global
  const label = (id: number): string => {
    const node = nodes.find((candidate) => candidate.id === id)
    return node === undefined ? '' : `${nodeCode(node, nodes)} ${node.name}`
  }
  const code = (id: number): string => {
    const node = nodes.find((candidate) => candidate.id === id)
    return node === undefined ? '' : nodeCode(node, nodes)
  }
  switch (target.type) {
    case 'nodeMute':
      return t('hotkey.targetMute', { node: label(target.node) })
    case 'nodeRecord':
      return t('hotkey.targetRecord', { node: label(target.node) })
    case 'routeToggle':
      return t('hotkey.targetRoute', { from: code(target.from), to: code(target.to) })
    case 'allRecording':
      return t('hotkey.allRecording')
    case 'panicMute':
      return t('hotkey.panicMute')
    case 'toggleWindow':
      return t('hotkey.toggleWindow')
  }
  return ''
}
