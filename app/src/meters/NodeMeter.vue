<script setup lang="ts">
import { onBeforeUnmount, onMounted, useTemplateRef, watch } from 'vue'
import { meterRenderer, type MeterOrientation } from './meterRenderer'

// 立体声电平表：canvas 交给 meterRenderer 统一绘制，dB 数字也由它直接写入。
// 横向用在工作台节点上，竖向用在控制台的通道条上。
const props = withDefaults(defineProps<{ node: number; muted: boolean; orientation?: MeterOrientation }>(), {
  orientation: 'horizontal',
})

const canvas = useTemplateRef<HTMLCanvasElement>('canvas')
const readout = useTemplateRef<HTMLElement>('readout')
let unregister: (() => void) | undefined

onMounted(() => {
  if (canvas.value !== null) {
    unregister = meterRenderer.register(props.node, canvas.value, readout.value, props.orientation)
  }
  meterRenderer.setMuted(props.node, props.muted)
})

watch(
  () => props.muted,
  (muted) => meterRenderer.setMuted(props.node, muted),
)

onBeforeUnmount(() => unregister?.())
</script>

<template>
  <div class="node-meter" :class="orientation">
    <canvas ref="canvas" class="bars" aria-hidden="true"></canvas>
    <span ref="readout" class="db">−∞</span>
  </div>
</template>

<style scoped>
.node-meter {
  display: flex;
  align-items: center;
  gap: 8px;
  flex: 1;
  min-width: 0;
}

.node-meter.vertical {
  flex-direction: column-reverse;
  gap: 4px;
  min-height: 0;
  height: 100%;
}

.vertical .bars {
  flex: 1;
  width: 15px;
  height: auto;
  min-height: 0;
}

.vertical .db {
  width: auto;
  height: 16px;
  line-height: 16px;
  text-align: center;
  color: var(--text-2);
  white-space: nowrap;
}

.bars {
  flex: 1;
  min-width: 0;
  height: 7px;
  border-radius: 3px;
}

.db {
  width: 44px;
  flex: none;
  text-align: right;
  font-size: var(--fontSizeBase100);
  color: var(--text-3);
  font-variant-numeric: tabular-nums;
}
</style>
