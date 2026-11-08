<script setup lang="ts">
import { onBeforeUnmount, onMounted, useTemplateRef, watch } from 'vue'
import { meterRenderer } from './meterRenderer'

// 节点上的立体声电平表：canvas 交给 meterRenderer 统一绘制，dB 数字也由它直接写入
const props = defineProps<{ node: number; muted: boolean }>()

const canvas = useTemplateRef<HTMLCanvasElement>('canvas')
const readout = useTemplateRef<HTMLElement>('readout')
let unregister: (() => void) | undefined

onMounted(() => {
  if (canvas.value !== null) {
    unregister = meterRenderer.register(props.node, canvas.value, readout.value)
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
  <div class="node-meter">
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
