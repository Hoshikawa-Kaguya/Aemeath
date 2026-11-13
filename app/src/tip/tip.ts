import { type Directive, shallowReactive } from 'vue'

// 悬停提示：替代原生 title。原生提示在 title 变化时会关掉，带实时数值的提示因此看不清；
// 全局只有一个悬浮窗（TipLayer.vue），v-tip 只登记文字，悬停、聚焦由 window 上的监听统一处理。

const SHOW_DELAY_MS = 200
/** 提示刚因移开而隐藏时，这段时间内移到别的元素上直接显示，不再等待 */
const SWITCH_GRACE_MS = 300

type TipValue = string | null | undefined

/** 当前显示的提示：anchor 为 null 时不显示 */
export const tip = shallowReactive({ anchor: null as HTMLElement | null, text: '' })

const texts = new WeakMap<HTMLElement, string>()
let hovered: HTMLElement | null = null
let pressed = false
let timer = 0
let warmUntil = 0

function isTruncated(el: HTMLElement): boolean {
  return el.scrollWidth > el.clientWidth
}

/** 提示和显示的文字相同时，只在文字被截断时才有必要显示 */
function wanted(el: HTMLElement): boolean {
  const text = texts.get(el) ?? ''
  return text !== '' && (el.textContent?.trim() !== text.trim() || isTruncated(el))
}

/** 从事件目标往上找第一个需要显示提示的元素，内层优先 */
function findTip(target: EventTarget | null): HTMLElement | null {
  let el = target instanceof Element ? target.closest<HTMLElement>('[data-tip]') : null
  while (el !== null && !wanted(el)) {
    el = el.parentElement?.closest<HTMLElement>('[data-tip]') ?? null
  }
  return el
}

function show(el: HTMLElement): void {
  window.clearTimeout(timer)
  tip.anchor = el
  tip.text = texts.get(el) ?? ''
}

function hide(): void {
  window.clearTimeout(timer)
  if (tip.anchor !== null) {
    warmUntil = performance.now() + SWITCH_GRACE_MS
  }
  tip.anchor = null
}

function onPointerOver(event: PointerEvent): void {
  const el = findTip(event.target)
  if (el === hovered) {
    return
  }
  hovered = el
  hide()
  if (el === null || pressed) {
    return
  }
  if (performance.now() < warmUntil) {
    show(el)
  } else {
    timer = window.setTimeout(() => show(el), SHOW_DELAY_MS)
  }
}

function onPointerOut(event: PointerEvent): void {
  if (event.relatedTarget === null) {
    hovered = null
    hide()
  }
}

function onPointerDown(): void {
  pressed = true
  hide()
}

function onPointerUp(): void {
  pressed = false
}

function onFocusIn(event: FocusEvent): void {
  // 只响应键盘聚焦：鼠标点击引起的聚焦不弹提示
  const target = event.target
  if (target instanceof HTMLElement && target.matches(':focus-visible')) {
    const el = findTip(target)
    if (el !== null) {
      show(el)
    }
  }
}

function onFocusOut(event: FocusEvent): void {
  if (tip.anchor !== null && event.target instanceof Node && tip.anchor.contains(event.target)) {
    hide()
  }
}

function onKeyDown(event: KeyboardEvent): void {
  if (event.key === 'Escape') {
    hide()
  }
}

/** 安装全局监听，应用启动时调用一次 */
export function installTips(): void {
  window.addEventListener('pointerover', onPointerOver, true)
  window.addEventListener('pointerout', onPointerOut, true)
  window.addEventListener('pointerdown', onPointerDown, true)
  window.addEventListener('pointerup', onPointerUp, true)
  window.addEventListener('pointercancel', onPointerUp, true)
  window.addEventListener('focusin', onFocusIn, true)
  window.addEventListener('focusout', onFocusOut, true)
  window.addEventListener('keydown', onKeyDown, true)
  // 滚动、缩放画布时锚点会移动，直接隐藏
  window.addEventListener('wheel', hide, { capture: true, passive: true })
  window.addEventListener('scroll', hide, true)
  window.addEventListener('resize', hide)
  window.addEventListener('blur', hide)
}

/** 读屏：没有文字的元素用提示作名称，有文字的作描述；和显示文字相同时不重复 */
function setAria(el: HTMLElement, text: string): void {
  const visible = el.textContent?.trim() ?? ''
  const own = el.dataset.tipAria
  if (own !== undefined) {
    el.removeAttribute(own)
    delete el.dataset.tipAria
  }
  if (text === '' || text.trim() === visible || el.getAttribute('aria-label') === text) {
    return
  }
  const attribute = visible === '' && !el.hasAttribute('aria-label') ? 'aria-label' : 'aria-description'
  el.setAttribute(attribute, text)
  el.dataset.tipAria = attribute
}

function update(el: HTMLElement, value: TipValue): void {
  const text = value ?? ''
  texts.set(el, text)
  setAria(el, text)
  if (tip.anchor === el) {
    tip.text = text
  }
}

/** 悬停提示，用法同 title：`v-tip="文字"` */
export const vTip: Directive<HTMLElement, TipValue> = {
  mounted(el, binding) {
    el.dataset.tip = ''
    update(el, binding.value)
  },
  updated(el, binding) {
    update(el, binding.value)
  },
  unmounted(el) {
    if (hovered === el) {
      hovered = null
      window.clearTimeout(timer)
    }
    if (tip.anchor === el) {
      hide()
    }
  },
}
