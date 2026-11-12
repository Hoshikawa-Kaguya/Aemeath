<script setup lang="ts">
import { ref, watch } from 'vue'
import AppIcon from '@/components/AppIcon.vue'

// 程序图标由后端的 icon 协议提供；没有路径或加载失败（例如 pnpm preview）时显示通用的应用图标
const props = defineProps<{ path: string | null | undefined }>()
const failed = ref(false)
watch(
  () => props.path,
  () => (failed.value = false),
)
</script>

<template>
  <img
    v-if="path && !failed"
    class="i"
    :src="`http://icon.localhost/${encodeURIComponent(path)}`"
    alt=""
    aria-hidden="true"
    @error="failed = true"
  />
  <AppIcon v-else name="apps" />
</template>
