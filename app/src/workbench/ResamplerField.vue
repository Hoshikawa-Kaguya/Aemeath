<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { ResamplerKind } from '@/api/types'

// 按节点覆盖的重采样算法：外部设备和应用源的节点设置共用。null 表示跟随全局默认。
const resampler = defineModel<ResamplerKind | null>({ required: true })
const { t } = useI18n()

const RESAMPLERS: ResamplerKind[] = ['sincHigh', 'sincStandard', 'polynomial', 'linear']
const FOLLOW_GLOBAL = 'global'

const value = computed({
  get: () => resampler.value ?? FOLLOW_GLOBAL,
  set: (choice: string) => {
    resampler.value = choice === FOLLOW_GLOBAL ? null : (choice as ResamplerKind)
  },
})
</script>

<template>
  <label class="field">
    <span>{{ t('device.resampler') }}</span>
    <select v-model="value" class="select">
      <option :value="FOLLOW_GLOBAL">{{ t('device.resamplerGlobal') }}</option>
      <option v-for="kind in RESAMPLERS" :key="kind" :value="kind">{{ t(`resampler.${kind}.name`) }}</option>
    </select>
    <span class="caption">
      {{ resampler === null ? t('device.resamplerGlobalHint') : t(`resampler.${resampler}.hint`) }}
    </span>
  </label>
</template>
