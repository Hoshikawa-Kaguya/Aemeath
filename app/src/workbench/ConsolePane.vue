<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, useTemplateRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import type { NodeView } from '@/api/types'
import AppIcon from '@/components/AppIcon.vue'
import { openMenu, type MenuItem } from '@/menu/contextMenu'
import { useAppStore } from '@/stores/app'
import { motionMs } from '@/theme/theme'
import StripCard from './StripCard.vue'

// 控制台（GLOSSARY：控制台、固定）：被固定的节点以竖排通道条呈现，顺序保存在配置里。
// 拖动通道条的标题排序：被拖的通道条浮起、只沿水平方向跟手，目标位置显示半透明的放置预览，其余通道条滑开让位。
// 选中节点后按 Alt+←/→ 也能排序（工作台的快捷键调用 moveBy）。
const props = defineProps<{
  nodeMenu: (node: NodeView) => MenuItem[]
  /** 与工作台同步的选中节点 */
  selected: number | null
  /** 控制台是否收起为标题栏 */
  collapsed: boolean
  /** 展开且有已固定节点时允许拖拽顶边调整高度 */
  resizable: boolean
  /** 当前渲染高度（CSS 像素），用于拖动时计算新高度 */
  height: number
}>()
const emit = defineEmits<{
  select: [node: NodeView]
  toggleCollapse: []
  resize: [height: number]
  resizeEnd: [height: number]
  resizeCancel: []
  resetHeight: []
}>()
const { t } = useI18n()
const store = useAppStore()
const strips = useTemplateRef<HTMLElement>('strips')
const resizing = ref(false)

/** 按下后移动超过这个距离（px）才开始拖动，单击或双击时的抖动不算 */
const DRAG_THRESHOLD_PX = 4

/** 拖拽顶边分隔条调整控制台高度；双击恢复默认高度 */
function onResizePointerDown(event: PointerEvent): void {
  if (!props.resizable || event.button !== 0) {
    return
  }
  event.preventDefault()
  const handle = event.currentTarget as HTMLElement
  handle.setPointerCapture(event.pointerId)
  const startY = event.clientY
  const startHeight = props.height
  let latestHeight = startHeight
  let moved = false
  resizing.value = true

  const onMove = (moveEvent: PointerEvent) => {
    if (!moved && Math.abs(moveEvent.clientY - startY) < DRAG_THRESHOLD_PX) {
      return
    }
    moved = true
    latestHeight = startHeight + (startY - moveEvent.clientY)
    emit('resize', latestHeight)
  }
  const cleanup = () => {
    handle.removeEventListener('pointermove', onMove)
    handle.removeEventListener('pointerup', onUp)
    handle.removeEventListener('pointercancel', onCancel)
    resizing.value = false
  }
  const onUp = () => {
    cleanup()
    if (moved) {
      emit('resizeEnd', latestHeight)
    }
  }
  const onCancel = () => {
    cleanup()
    if (moved) {
      emit('resizeCancel')
    }
  }
  handle.addEventListener('pointermove', onMove)
  handle.addEventListener('pointerup', onUp)
  handle.addEventListener('pointercancel', onCancel)
}

/** 指针离控制台左右边缘这么近（px）时自动滚动，越近越快 */
const EDGE_PX = 48
/** 自动滚动的最大速度（px / 帧） */
const MAX_SCROLL_PX = 16

/** 已提交给后端、还没收到新快照时先按这个顺序显示，免得通道条闪回原位；后端失败时清掉，回到原来的顺序 */
const localOrder = ref<number[] | null>(null)
/** 只有最后一次提交返回时才清掉 localOrder，连按 Alt+→ 时不会闪回中间的顺序 */
let saveTicket = 0
/** 已固定且仍存在的节点，按保存的顺序 */
const order = computed(() => (localOrder.value ?? store.state?.console ?? []).filter((id) => store.nodeById.has(id)))
const unpinned = computed(() => store.nodes.filter((node) => !(store.state?.console ?? []).includes(node.id)))

interface Drag {
  node: number
  /** 原来的位置 */
  from: number
  /** 放置预览的位置 */
  target: number
  /** 浮起的通道条的位置和大小（视口坐标），纵向和大小在拖动中不变 */
  left: number
  top: number
  width: number
  height: number
  /** 按下处离通道条左边缘的距离 */
  grab: number
  /** 已经松开或取消，正滑向放置预览 */
  settling: boolean
}

const drag = ref<Drag | null>(null)
const dragNode = computed(() => (drag.value === null ? undefined : store.nodeById.get(drag.value.node)))
/** 按下了标题、还没越过拖动阈值 */
let press: { node: number; x: number } | null = null
let pointerX = 0
let frame = 0
/** 拖动开始时量好：除被拖的那条以外，各通道条的宽度（按顺序）、间距和左内边距 */
let otherWidths: number[] = []
let gap = 0
let padStart = 0

/** 把节点插到 ids 的第 index 位（先去掉它原来的位置） */
function placed(ids: number[], node: number, index: number): number[] {
  const rest = ids.filter((id) => id !== node)
  return [...rest.slice(0, index), node, ...rest.slice(index)]
}

const displayed = computed(() =>
  drag.value === null ? order.value : placed(order.value, drag.value.node, drag.value.target),
)
const pinned = computed(() =>
  displayed.value.flatMap((id) => {
    const node = store.nodeById.get(id)
    return node === undefined ? [] : [node]
  }),
)

function pinMenu(event: MouseEvent): void {
  const items: MenuItem[] =
    unpinned.value.length === 0
      ? [{ kind: 'action', label: t('con.allPinned'), disabled: true, run: () => {} }]
      : unpinned.value.map((node) => ({
          kind: 'action',
          label: node.name,
          run: () => void store.run(() => backend.pinNode(node.id, true)),
        }))
  openMenu(event, items)
}

function slotOf(node: number): HTMLElement | null {
  return strips.value?.querySelector<HTMLElement>(`[data-node="${node}"]`) ?? null
}

/** 先按新顺序显示，再提交给后端 */
function save(node: number, index: number): void {
  localOrder.value = placed(order.value, node, index)
  const ticket = ++saveTicket
  void store
    .run(() => backend.movePin(node, index))
    .finally(() => {
      if (ticket === saveTicket) {
        localOrder.value = null
      }
    })
}

/** 横向滚动控制台，让这条通道条完整可见。按布局位置算，不受让位动画的位移影响 */
function reveal(el: HTMLElement, smooth: boolean): void {
  const container = strips.value
  if (container === null) {
    return
  }
  const pad = parseFloat(getComputedStyle(container).paddingLeft) || 0
  const left = el.offsetLeft - pad
  const right = el.offsetLeft + el.offsetWidth + pad - container.clientWidth
  const scroll = Math.min(Math.max(container.scrollLeft, right), left)
  if (scroll !== container.scrollLeft) {
    container.scrollTo({ left: scroll, behavior: smooth && motionMs('--dur') > 0 ? 'smooth' : 'auto' })
  }
}

/** 把已固定的节点左右移动 step 位，返回节点是否在控制台里 */
function moveBy(node: number, step: number): boolean {
  const from = order.value.indexOf(node)
  if (from < 0) {
    return false
  }
  const index = Math.min(Math.max(from + step, 0), order.value.length - 1)
  if (index !== from && drag.value === null) {
    save(node, index)
    void nextTick(() => {
      const el = slotOf(node)
      if (el !== null) {
        reveal(el, true)
      }
    })
  }
  return true
}

defineExpose({ moveBy })

/** 在通道条任何位置按下都会选中它，不拦截事件；只有按在标题上（"更多"按钮除外）才可能开始拖动 */
function onPointerDown(event: PointerEvent, node: NodeView): void {
  emit('select', node)
  const target = event.target
  if (
    event.button !== 0 ||
    drag.value !== null ||
    !(target instanceof Element) ||
    target.closest('.strip-head') === null ||
    target.closest('button') !== null
  ) {
    return
  }
  press = { node: node.id, x: event.clientX }
  window.addEventListener('pointermove', onPointerMove)
  window.addEventListener('pointerup', onPointerUp)
  window.addEventListener('pointercancel', onPointerCancel)
  window.addEventListener('keydown', onKeyDown, true)
}

function removeListeners(): void {
  window.removeEventListener('pointermove', onPointerMove)
  window.removeEventListener('pointerup', onPointerUp)
  window.removeEventListener('pointercancel', onPointerCancel)
  window.removeEventListener('keydown', onKeyDown, true)
}

function onPointerMove(event: PointerEvent): void {
  pointerX = event.clientX
  if (press !== null && drag.value === null && Math.abs(pointerX - press.x) > DRAG_THRESHOLD_PX) {
    begin(press.node, press.x)
  }
  if (drag.value?.settling === false) {
    follow()
  }
}

function onPointerUp(): void {
  finish(true)
}

function onPointerCancel(): void {
  finish(false)
}

/** 拖动中按 Esc 取消；不让它再传到工作台去取消选中 */
function onKeyDown(event: KeyboardEvent): void {
  if (event.key === 'Escape' && drag.value?.settling === false) {
    event.preventDefault()
    event.stopImmediatePropagation()
    finish(false)
  }
}

function begin(node: number, startX: number): void {
  const el = slotOf(node)
  const container = strips.value
  if (el === null || container === null) {
    return
  }
  const rect = el.getBoundingClientRect()
  const style = getComputedStyle(container)
  gap = parseFloat(style.columnGap) || 0
  padStart = parseFloat(style.paddingLeft) || 0
  otherWidths = order.value.filter((id) => id !== node).map((id) => slotOf(id)?.offsetWidth ?? 0)
  const from = order.value.indexOf(node)
  drag.value = {
    node,
    from,
    target: from,
    left: rect.left,
    top: rect.top,
    width: rect.width,
    height: rect.height,
    grab: startX - rect.left,
    settling: false,
  }
  frame = requestAnimationFrame(autoScroll)
}

/**
 * 放置位置：浮起的通道条左边缘越过了几条其他通道条的中线。
 * 中线按去掉被拖的那条之后的排列计算，与当前预览在哪里无关，所以不会在两个位置之间来回跳。
 */
function dropIndex(left: number): number {
  let x = padStart
  let index = 0
  for (const width of otherWidths) {
    if (left <= x + width / 2) {
      break
    }
    x += width + gap
    index += 1
  }
  return index
}

/** 浮起的通道条跟着指针水平移动（不移出控制台），再按它的位置更新放置预览 */
function follow(): void {
  const current = drag.value
  const container = strips.value
  if (current === null || container === null) {
    return
  }
  const box = container.getBoundingClientRect()
  current.left = Math.min(Math.max(pointerX - current.grab, box.left), box.right - current.width)
  current.target = dropIndex(current.left - box.left - container.clientLeft + container.scrollLeft)
}

/** 离边缘的距离对应的滚动速度：进入边缘区域后越近越快 */
function edgeSpeed(distance: number): number {
  return distance >= EDGE_PX ? 0 : Math.ceil(MAX_SCROLL_PX * (1 - Math.max(distance, 0) / EDGE_PX))
}

/** 指针停在边缘附近不动时也要继续滚，所以每帧检查一次 */
function autoScroll(): void {
  const current = drag.value
  const container = strips.value
  if (current === null || current.settling || container === null) {
    return
  }
  const box = container.getBoundingClientRect()
  const speed = edgeSpeed(box.right - pointerX) - edgeSpeed(pointerX - box.left)
  if (speed !== 0) {
    container.scrollLeft += speed
    follow()
  }
  frame = requestAnimationFrame(autoScroll)
}

/** 结束拖动：提交时停在放置预览的位置，取消时回到原位。浮起的通道条滑到那里以后再收起 */
function finish(commit: boolean): void {
  removeListeners()
  press = null
  const current = drag.value
  if (current === null || current.settling) {
    return
  }
  cancelAnimationFrame(frame)
  if (!commit) {
    current.target = current.from
  }
  current.settling = true
  if (current.target !== current.from && order.value.includes(current.node)) {
    save(current.node, current.target)
  }
  void nextTick(() => {
    const el = slotOf(current.node)
    const container = strips.value
    if (el !== null && container !== null) {
      reveal(el, false)
      const box = container.getBoundingClientRect()
      current.left = box.left + container.clientLeft + el.offsetLeft - container.scrollLeft
    }
    setTimeout(() => (drag.value = null), motionMs('--dur'))
  })
}

onBeforeUnmount(() => {
  removeListeners()
  cancelAnimationFrame(frame)
})
</script>

<template>
  <section
    class="console-pane"
    :class="{ collapsed: props.collapsed, empty: pinned.length === 0 }"
    :aria-label="t('con.title')"
  >
    <div
      v-if="props.resizable"
      v-tip="t('con.resizeTip')"
      class="resize-handle"
      :class="{ active: resizing }"
      role="separator"
      aria-orientation="horizontal"
      :aria-label="t('con.resizeTip')"
      @pointerdown="onResizePointerDown"
      @dblclick="emit('resetHeight')"
    ></div>
    <div class="pane-head">
      <h2>{{ t('con.title') }}</h2>
      <span class="caption ellipsis">{{ t('con.caption', { n: order.length }) }}</span>
      <div class="spacer"></div>
      <fluent-button appearance="subtle" @click="pinMenu">
        <AppIcon slot="start" name="plus" />{{ t('con.pin') }}
      </fluent-button>
      <button
        v-tip="t(props.collapsed ? 'con.expand' : 'con.collapse')"
        type="button"
        class="collapse-btn"
        :aria-expanded="!props.collapsed"
        :aria-label="t(props.collapsed ? 'con.expand' : 'con.collapse')"
        @click="emit('toggleCollapse')"
      >
        <AppIcon :name="props.collapsed ? 'chevron-up' : 'chevron-down'" />
      </button>
    </div>
    <div v-if="!props.collapsed" ref="strips" class="strips" :class="{ 'strips-empty': pinned.length === 0 }">
      <p v-if="pinned.length === 0" class="caption empty-text">{{ t('con.empty') }}</p>
      <TransitionGroup v-else name="slot">
        <div
          v-for="node in pinned"
          :key="node.id"
          class="slot"
          :class="{ sel: node.id === props.selected, placeholder: drag?.node === node.id }"
          :data-node="node.id"
          @pointerdown="onPointerDown($event, node)"
        >
          <StripCard :node="node" @menu="openMenu($event, props.nodeMenu(node))" />
        </div>
      </TransitionGroup>
    </div>
    <StripCard
      v-if="!props.collapsed && drag !== null && dragNode !== undefined"
      class="floating"
      :class="{ settling: drag.settling }"
      :node="dragNode"
      :style="{ left: `${drag.left}px`, top: `${drag.top}px`, width: `${drag.width}px`, height: `${drag.height}px` }"
      inert
      aria-hidden="true"
    />
  </section>
</template>

<style scoped>
.console-pane {
  position: relative;
  display: flex;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
}

.console-pane.collapsed .pane-head {
  border-bottom: 0;
}

/* 顶边可拖拽分隔条：命中高度比视觉线宽，便于瞄准 */
.resize-handle {
  position: absolute;
  left: 0;
  right: 0;
  top: -4px;
  height: 8px;
  z-index: 6;
  cursor: ns-resize;
}

.resize-handle::after {
  content: '';
  position: absolute;
  left: 0;
  right: 0;
  top: 3px;
  height: 2px;
  background: transparent;
  transition: background-color var(--dur-fast) linear;
}

.resize-handle:hover::after,
.resize-handle.active::after {
  background: var(--accent);
}

.spacer {
  flex: 1;
}

.collapse-btn {
  display: grid;
  place-items: center;
  flex: none;
  width: 28px;
  height: 28px;
  padding: 0;
  border: 0;
  border-radius: var(--r-ctl);
  background: transparent;
  color: var(--text-2);
  cursor: pointer;
}

.collapse-btn:hover,
.collapse-btn:focus-visible {
  background: var(--card-hover);
  color: var(--text);
  outline: none;
}

.collapse-btn svg {
  width: 16px;
  height: 16px;
}

/* position: relative 让通道条的 offsetLeft 相对这里计算 */
.strips {
  position: relative;
  flex: 1;
  display: flex;
  gap: 8px;
  min-height: 0;
  padding: 10px 12px 12px;
  overflow-x: auto;
  overflow-y: hidden;
}

.strips.strips-empty {
  align-items: center;
  padding: 0 16px;
}

.empty-text {
  margin: 0;
}

.slot {
  display: flex;
  border-radius: var(--r-card);
}

.slot.sel {
  outline: 2px solid var(--accent);
  outline-offset: 1px;
}

.slot.placeholder {
  opacity: 0.4;
}

/* 其他通道条让位、键盘移动时的滑动 */
.slot-move {
  transition: transform var(--dur) var(--ease-move);
}

/* 前缀提高优先级，盖过通道条自己半透明的卡片底色 */
.console-pane .floating {
  position: fixed;
  /* 宽高取自通道条的外框，包含内边距和边框 */
  box-sizing: border-box;
  pointer-events: none;
  background: var(--card-solid);
  box-shadow: var(--shadow16);
}

.console-pane .floating.settling {
  transition: left var(--dur) var(--ease-move);
}
</style>
