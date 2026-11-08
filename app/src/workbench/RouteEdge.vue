<script setup lang="ts">
import { computed, inject, ref } from 'vue'
import { BaseEdge, EdgeLabelRenderer, type EdgeProps } from '@vue-flow/core'
import { useI18n } from 'vue-i18n'
import AppIcon from '@/components/AppIcon.vue'
import { formatGain } from '@/format/format'
import { nearestT, pointAt, routeCurve } from './curve'
import { workbenchKey, type RouteData } from './context'

// 连线（原型的 #wires 与 .wire-label）：开启为强调色实线、关闭为虚线；标签上的圆点开关连线，
// 滚轮调增益；选中后可以按住标签沿连线拖动，避免标签互相重叠（位置随连线保存）。
const props = defineProps<EdgeProps<RouteData>>()
const { t } = useI18n()
const actions = inject(workbenchKey)

/** 按下后移动超过这么多像素才算拖动标签，否则是单击 */
const DRAG_THRESHOLD = 4
/** 透明的命中路径宽度 */
const HIT_WIDTH = 14

const route = computed(() => props.data.route)
const curve = computed(() => routeCurve({ x: props.sourceX, y: props.sourceY }, { x: props.targetX, y: props.targetY }))
/** 拖动中的标签位置；不在拖动时用连线保存的位置 */
const dragPosition = ref<number | null>(null)
const labelPoint = computed(() => pointAt(curve.value, dragPosition.value ?? route.value.labelPosition))
const gainText = computed(() =>
  route.value.enabled ? formatGain(route.value.gainDb) : t('route.off', { gain: formatGain(route.value.gainDb) }),
)
const names = computed(() => ({ src: props.data.sourceName, dst: props.data.targetName }))

function onWheel(event: WheelEvent): void {
  actions?.nudgeGain(route.value, event.deltaY < 0 ? 1 : -1)
}

function onLabelPointerDown(event: PointerEvent): void {
  actions?.selectRoute(route.value)
  if (!props.data.selected) {
    return
  }
  const start = { x: event.clientX, y: event.clientY }
  const label = event.currentTarget as HTMLElement
  label.setPointerCapture(event.pointerId)
  let moved = false
  const move = (moveEvent: PointerEvent) => {
    if (!moved && Math.hypot(moveEvent.clientX - start.x, moveEvent.clientY - start.y) < DRAG_THRESHOLD) {
      return
    }
    moved = true
    if (actions !== undefined) {
      dragPosition.value = nearestT(curve.value, actions.toFlow({ x: moveEvent.clientX, y: moveEvent.clientY }))
    }
  }
  const up = () => {
    label.removeEventListener('pointermove', move)
    label.removeEventListener('pointerup', up)
    label.removeEventListener('pointercancel', up)
    if (moved && dragPosition.value !== null) {
      actions?.moveLabel(route.value, dragPosition.value)
    }
    dragPosition.value = null
  }
  label.addEventListener('pointermove', move)
  label.addEventListener('pointerup', up)
  label.addEventListener('pointercancel', up)
}
</script>

<template>
  <BaseEdge
    :id="id"
    :path="curve.path"
    :interaction-width="HIT_WIDTH"
    class="route-path"
    :class="{ on: route.enabled, off: !route.enabled, sel: data.selected }"
  />
  <EdgeLabelRenderer>
    <div
      class="wire-label nodrag nopan"
      :class="{ on: route.enabled, off: !route.enabled, sel: data.selected, dragging: dragPosition !== null }"
      :style="{ transform: `translate(-50%, -50%) translate(${labelPoint.x}px, ${labelPoint.y}px)` }"
      :title="data.selected ? t('route.moveHint') : undefined"
      @pointerdown.stop="onLabelPointerDown"
      @wheel.stop.prevent="onWheel"
    >
      <span v-if="data.selected" class="grip" aria-hidden="true"><AppIcon name="grip" /></span>
      <button
        type="button"
        class="toggle"
        :aria-pressed="route.enabled"
        :aria-label="t('route.toggle', names)"
        @pointerdown.stop
        @click="actions?.toggleRoute(route)"
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

.vue-flow__edge:hover .vue-flow__edge-path.route-path {
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
  height: 22px;
  padding: 0 2px;
  border-radius: var(--borderRadiusCircular);
  border: 1px solid var(--stroke-strong);
  background: var(--card-solid);
  color: var(--text-2);
  white-space: nowrap;
  font-size: var(--fontSizeBase100);
  font-variant-numeric: tabular-nums;
  pointer-events: all;
}

.wire-label button {
  height: 18px;
  padding: 0 6px;
  border: 0;
  border-radius: var(--borderRadiusCircular);
  background: transparent;
  color: inherit;
  font: inherit;
  line-height: 18px;
  cursor: pointer;
  display: inline-flex;
  align-items: center;
}

.wire-label button:hover {
  background: var(--card-hover);
}

.toggle {
  padding: 0 3px;
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

.wire-label.sel {
  outline: 2px solid var(--accent);
  outline-offset: 1px;
  z-index: 4;
  cursor: grab;
}

.wire-label.sel .gain {
  cursor: grab;
}

.wire-label.dragging,
.wire-label.dragging * {
  cursor: grabbing;
}

.grip {
  display: inline-flex;
  width: 12px;
  height: 18px;
  align-items: center;
  justify-content: center;
  color: var(--text-3);
}

.grip svg {
  width: 10px;
  height: 14px;
}
</style>
