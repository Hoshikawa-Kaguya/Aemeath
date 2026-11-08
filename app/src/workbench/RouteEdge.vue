<script setup lang="ts">
import { computed, inject, onBeforeUnmount, ref } from 'vue'
import { BaseEdge, EdgeLabelRenderer, type EdgeProps } from '@vue-flow/core'
import { useI18n } from 'vue-i18n'
import { formatGain } from '@/format/format'
import { isMenuKey } from '@/menu/contextMenu'
import { nearestT, pointAt, routeCurve } from './curve'
import { createHoverWheel } from './hoverWheel'
import { workbenchKey, type RouteData } from './context'

// 连线（原型的 #wires 与 .wire-label）：开启为强调色实线、关闭为虚线。
// 标签上的交互（M2 计划 2.2，维护者 2026-11-08 要求防误触、拖动直观）：
// - 单击标签选中连线；单击圆点开关连线。只有"按下和松开都在圆点上、没有移动"才算单击圆点。
// - 按下后一移动就开始拖动（不必长按、不必先选中）。标签只在连线上滑动：指针停在哪，标签就滑到连线上离它最近的点
//   （位置随连线保存）。
// - 悬停在标签上滚动即调增益，不必先选中；滚轮缩放工作台时指针扫过标签不会改增益（hoverWheel.ts）。
//   按住 Ctrl 时总是调增益。
const props = defineProps<EdgeProps<RouteData>>()
const { t } = useI18n()
const actions = inject(workbenchKey)

/** 按下后移动超过这么多像素就开始拖动，否则松开时算单击 */
const MOVE_TOLERANCE = 4
/** 透明的命中路径宽度 */
const HIT_WIDTH = 14
const PRIMARY_BUTTON = 0

const route = computed(() => props.data.route)
const curve = computed(() => routeCurve({ x: props.sourceX, y: props.sourceY }, { x: props.targetX, y: props.targetY }))
/** 拖动中标签在连线上的位置；不在拖动时用连线保存的位置 */
const dragPosition = ref<number | null>(null)
const labelPoint = computed(() => pointAt(curve.value, dragPosition.value ?? route.value.labelPosition))
const gainText = computed(() =>
  route.value.enabled ? formatGain(route.value.gainDb) : t('route.off', { gain: formatGain(route.value.gainDb) }),
)
const names = computed(() => ({ src: props.data.sourceName, dst: props.data.targetName }))

let release: (() => void) | null = null

const hoverWheel = createHoverWheel()

/** 悬停在标签上滚动即调增益；滚轮缩放工作台时扫过标签不算（见 hoverWheel.ts）。按住 Ctrl 时总是调增益 */
function onWheel(event: WheelEvent): void {
  if (!hoverWheel.accepts() && !event.ctrlKey) {
    return
  }
  event.preventDefault()
  event.stopPropagation()
  actions?.nudgeGain(route.value, event.deltaY < 0 ? 1 : -1)
}

function onLabelPointerDown(event: PointerEvent): void {
  if (event.button !== PRIMARY_BUTTON) {
    return
  }
  const start = { x: event.clientX, y: event.clientY }
  const startedOnToggle = (event.target as Element).closest('.toggle') !== null
  const label = event.currentTarget as HTMLElement
  label.setPointerCapture(event.pointerId)
  let dragging = false

  const move = (moveEvent: PointerEvent) => {
    if (!dragging && Math.hypot(moveEvent.clientX - start.x, moveEvent.clientY - start.y) <= MOVE_TOLERANCE) {
      return
    }
    dragging = true
    if (actions !== undefined) {
      // 标签滑到连线上离指针最近的点
      dragPosition.value = nearestT(curve.value, actions.toFlow({ x: moveEvent.clientX, y: moveEvent.clientY }))
    }
  }
  const up = (upEvent: PointerEvent) => {
    cleanup()
    if (dragging) {
      if (dragPosition.value !== null && dragPosition.value !== route.value.labelPosition) {
        actions?.moveLabel(route.value, dragPosition.value)
      }
    } else if (upEvent.type === 'pointerup') {
      const endedOnToggle =
        (document.elementFromPoint(upEvent.clientX, upEvent.clientY)?.closest('.toggle') ?? null) !== null
      if (startedOnToggle && endedOnToggle) {
        actions?.toggleRoute(route.value)
      } else {
        actions?.selectRoute(route.value)
      }
    }
    dragPosition.value = null
  }
  const cleanup = () => {
    label.removeEventListener('pointermove', move)
    label.removeEventListener('pointerup', up)
    label.removeEventListener('pointercancel', up)
    release = null
  }
  release = cleanup
  label.addEventListener('pointermove', move)
  label.addEventListener('pointerup', up)
  label.addEventListener('pointercancel', up)
}

/** 圆点的键盘操作（空格或回车）。鼠标单击由上面的指针逻辑处理，这里忽略（detail > 0）。 */
function onToggleClick(event: MouseEvent): void {
  if (event.detail === 0) {
    actions?.toggleRoute(route.value)
  }
}

function onGainClick(event: MouseEvent): void {
  if (event.detail === 0) {
    actions?.selectRoute(route.value)
  }
}

function openMenu(event: MouseEvent | KeyboardEvent): void {
  actions?.selectRoute(route.value)
  actions?.openRouteMenu(event, route.value)
}

function onKeyDown(event: KeyboardEvent): void {
  if (isMenuKey(event)) {
    openMenu(event)
  }
}

onBeforeUnmount(() => release?.())
</script>

<template>
  <BaseEdge
    :id="id"
    :path="curve.path"
    :interaction-width="HIT_WIDTH"
    class="route-path"
    :class="{ on: route.enabled, off: !route.enabled, sel: data.selected, hot: data.highlighted }"
  />
  <EdgeLabelRenderer>
    <div
      class="wire-label nodrag nopan"
      :class="{
        on: route.enabled,
        off: !route.enabled,
        sel: data.selected,
        dragging: dragPosition !== null,
        duplicate: route.duplicate !== null,
      }"
      :style="{
        transform: `translate(-50%, -50%) translate(${labelPoint.x}px, ${labelPoint.y}px) scale(var(--label-lift))`,
      }"
      :title="
        route.duplicate === null
          ? t('route.moveHint')
          : t('node.duplicate', { program: route.duplicate.program, destination: route.duplicate.destination })
      "
      @pointerdown.stop="onLabelPointerDown"
      @pointerenter="hoverWheel.enter()"
      @wheel="onWheel"
      @contextmenu="openMenu"
      @keydown="onKeyDown"
    >
      <button
        type="button"
        class="toggle"
        :aria-pressed="route.enabled"
        :aria-label="t('route.toggle', names)"
        @click="onToggleClick"
      >
        <i></i>
      </button>
      <button
        type="button"
        class="gain"
        :aria-pressed="data.selected"
        :aria-label="
          t('route.select', {
            ...names,
            state: t(route.enabled ? 'route.stateOn' : 'route.stateOff'),
            gain: formatGain(route.gainDb),
          })
        "
        @click="onGainClick"
      >
        {{ gainText }}
      </button>
    </div>
  </EdgeLabelRenderer>
</template>

<style>
/* 连线路径由 Vue Flow 渲染在它自己的 SVG 里，scoped 样式碰不到，这里用全局样式 */
.vue-flow__edge-path.route-path {
  fill: none;
  stroke-width: 2;
  transition: stroke var(--dur) linear;
}

.vue-flow__edge-path.route-path.on {
  stroke: var(--accent);
}

.vue-flow__edge-path.route-path.off {
  stroke: var(--wire-off);
  stroke-dasharray: 5 5;
}

.vue-flow__edge:hover .vue-flow__edge-path.route-path,
.vue-flow__edge-path.route-path.hot {
  stroke-width: 3;
}

.vue-flow__edge-path.route-path.sel {
  stroke-width: 3.5;
  filter: drop-shadow(0 0 4px var(--accent));
}

.vue-flow__edge-path.route-path.off.sel {
  stroke: var(--text-2);
}
</style>

<style scoped>
.wire-label {
  position: absolute;
  z-index: 2;
  display: flex;
  align-items: center;
  gap: 2px;
  height: 24px;
  padding: 0 2px;
  border-radius: var(--borderRadiusCircular);
  border: 1px solid var(--stroke-strong);
  background: var(--card-solid);
  color: var(--text-2);
  white-space: nowrap;
  font-size: var(--fontSizeBase100);
  font-variant-numeric: tabular-nums;
  pointer-events: all;
  /* 位置用 transform 设定，拖动时不能有过渡，所以只过渡 --label-lift。
     放大必须写在 transform 的最后一项：独立的 scale 属性作用在整个 transform 之外，会把位置的平移一起放大，
     拖动时标签就偏到连线一侧 */
  --label-lift: 1;
  transition:
    --label-lift var(--dur-fast) var(--ease),
    box-shadow var(--dur-fast) var(--ease);
}

/* 注册后 --label-lift 才能过渡 */
@property --label-lift {
  syntax: '<number>';
  inherits: false;
  initial-value: 1;
}

.wire-label button {
  height: 20px;
  padding: 0 6px;
  border: 0;
  border-radius: var(--borderRadiusCircular);
  background: transparent;
  color: inherit;
  font: inherit;
  line-height: 20px;
  cursor: pointer;
  display: inline-flex;
  align-items: center;
}

.wire-label button:hover {
  background: var(--card-hover);
}

/* 圆点的命中区域比圆点本身大得多，并与增益数字之间留出间距，减少误触 */
.toggle {
  justify-content: center;
  min-width: 22px;
  padding: 0 5px;
}

.toggle i {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  border: 1.5px solid currentColor;
}

.wire-label.on {
  border-color: color-mix(in srgb, var(--accent) 60%, transparent);
  color: var(--text);
}

.wire-label.on .toggle i {
  background: var(--accent);
  border-color: var(--accent);
}

.wire-label.off {
  border-style: dashed;
  color: var(--text-3);
}

/* 重复路由只警告：描边标出，不改变开关的颜色 */
.wire-label.duplicate {
  border-color: var(--warn);
}

.wire-label.sel {
  outline: 2px solid var(--accent);
  outline-offset: 1px;
  z-index: 4;
}

/* 拖动中：略微浮起 */
.wire-label.dragging {
  z-index: 5;
  --label-lift: 1.08;
  box-shadow: var(--shadow16);
}

.wire-label.dragging,
.wire-label.dragging * {
  cursor: grabbing;
}
</style>
