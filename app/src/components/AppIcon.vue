<script setup lang="ts">
// 引用 IconSprite 中的图标；装饰性图标，对读屏器隐藏
export type IconName =
  | 'logo'
  | 'graph'
  | 'slots'
  | 'gear'
  | 'mic'
  | 'spk'
  | 'bus'
  | 'virt'
  | 'warn'
  | 'x'
  | 'plus'
  | 'fit'
  | 'check'
  | 'unplug'
  | 'sync'
  | 'info'
  | 'lang'
  | 'undo'
  | 'grip'

defineProps<{ name: IconName }>()
</script>

<template>
  <svg class="i" aria-hidden="true"><use :href="`#i-${name}`" /></svg>
</template>
