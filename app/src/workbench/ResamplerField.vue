<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { Resampler } from '@/api/types'

// 重采样算法和重采样预设。节点设置、添加节点和设置页共用；参数不对用户开放。
// null 表示跟随全局默认，设置页的全局默认没有这一项（allowFollow 为 false）。
type Algorithm = Resampler['algorithm']

interface PresetChoice {
  id: string
  /** 固定英文名（专有名词，不进 i18n）；没有时用 i18n 键 */
  fixedName?: string
  latency: string
}

const resampler = defineModel<Resampler | null>({ required: true })
const props = withDefaults(defineProps<{ allowFollow?: boolean }>(), { allowFollow: true })
const { t } = useI18n()

const FOLLOW_GLOBAL = 'global'
/** 算法名是专有名词，各语言都显示同样的文字，不进 i18n */
const ALGORITHMS: { id: Algorithm; name: string }[] = [
  { id: 'polyAsync', name: 'Rubato Poly Async' },
  { id: 'sincAsync', name: 'Rubato Sinc Async' },
  { id: 'fftSync', name: 'Rubato FFT Sync' },
]
/** 延迟是 48 kHz 下的实测值 */
const POLY_LATENCY = '<0.1'
const PRESETS: Record<Algorithm, PresetChoice[]> = {
  polyAsync: [
    { id: 'fastest', latency: POLY_LATENCY },
    { id: 'smooth', latency: POLY_LATENCY },
    { id: 'fine', latency: POLY_LATENCY },
  ],
  sincAsync: [
    { id: 'veryFast', fixedName: 'VeryFast', latency: '0.7' },
    { id: 'fast', fixedName: 'Fast', latency: '1.3' },
    { id: 'balanced', fixedName: 'Balanced', latency: '2.0' },
    { id: 'accurate', fixedName: 'Accurate', latency: '2.7' },
    { id: 'ultra', fixedName: 'Ultra', latency: '5.3' },
  ],
  fftSync: [
    { id: 'lowLatency', latency: '8' },
    { id: 'balanced', latency: '32' },
  ],
}
const DEFAULT_PRESETS = {
  polyAsync: 'fine',
  sincAsync: 'balanced',
  fftSync: 'lowLatency',
} as const

const algorithm = computed({
  get: () => resampler.value?.algorithm ?? FOLLOW_GLOBAL,
  set: (choice: string) => {
    resampler.value =
      choice === FOLLOW_GLOBAL
        ? null
        : ({ algorithm: choice, preset: DEFAULT_PRESETS[choice as Algorithm] } as Resampler)
  },
})
const preset = computed({
  get: () => resampler.value?.preset ?? '',
  set: (choice: string) => {
    if (resampler.value !== null) {
      resampler.value = { algorithm: resampler.value.algorithm, preset: choice } as Resampler
    }
  },
})
const presets = computed(() => (resampler.value === null ? [] : PRESETS[resampler.value.algorithm]))
const latency = computed(() => presets.value.find((choice) => choice.id === preset.value)?.latency ?? '')

function presetName(choice: PresetChoice): string {
  return choice.fixedName ?? t(`resampler.preset.${resampler.value?.algorithm}.${choice.id}`)
}
</script>

<template>
  <div class="fields">
    <label class="field">
      <span>{{ t('device.resampler') }}</span>
      <select v-model="algorithm" class="select">
        <button><selectedcontent></selectedcontent></button>
        <option v-if="props.allowFollow" :value="FOLLOW_GLOBAL">{{ t('device.resamplerGlobal') }}</option>
        <option v-for="choice in ALGORITHMS" :key="choice.id" :value="choice.id">{{ choice.name }}</option>
      </select>
      <span v-if="resampler === null" class="caption">{{ t('device.resamplerGlobalHint') }}</span>
    </label>
    <label v-if="resampler !== null" class="field">
      <span>{{ t('resampler.preset.label') }}</span>
      <select v-model="preset" class="select">
        <button><selectedcontent></selectedcontent></button>
        <option v-for="choice in presets" :key="choice.id" :value="choice.id">{{ presetName(choice) }}</option>
      </select>
      <span class="caption">{{ t('resampler.latency', { ms: latency }) }}</span>
      <span v-if="resampler.algorithm === 'polyAsync'" class="caption warning">{{ t('resampler.polyWarning') }}</span>
    </label>
  </div>
</template>

<style scoped>
.fields {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.warning {
  color: var(--warn);
}
</style>
