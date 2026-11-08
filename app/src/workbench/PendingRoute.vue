<script setup lang="ts">
import { computed } from 'vue'
import { routeCurve } from './curve'

// 拖线中的虚线（原型的 path.pending）。从输出端口拖时向右引出，从输入端口反向拖时从左侧进入。
const props = defineProps<{
  sourceX: number
  sourceY: number
  targetX: number
  targetY: number
  fromOutput: boolean
  bad: boolean
}>()

const path = computed(() => {
  const handle = { x: props.sourceX, y: props.sourceY }
  const pointer = { x: props.targetX, y: props.targetY }
  return (props.fromOutput ? routeCurve(handle, pointer) : routeCurve(pointer, handle)).path
})
</script>

<template>
  <path class="pending" :class="{ bad }" :d="path" />
</template>

<style scoped>
.pending {
  fill: none;
  stroke: var(--accent);
  stroke-dasharray: 6 4;
  stroke-width: 2;
}

.pending.bad {
  stroke: var(--crit);
}
</style>
