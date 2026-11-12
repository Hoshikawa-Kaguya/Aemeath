<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { FormatView, Resampler, SampleFormat, SlotSettings } from '@/api/types'
import { formatKhz } from '@/format/format'
import { useAppStore } from '@/stores/app'
import ResamplerField from './ResamplerField.vue'

// 占用槽位的节点的格式与重采样算法：配置属于节点，换槽位时带到新槽位。
const settings = defineModel<SlotSettings>({ required: true })
const { t } = useI18n()
const store = useAppStore()

/** 共享模式下虚拟设备可用的采样率 */
const RATES = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000]
const SAMPLE_FORMATS: SampleFormat[] = ['int16', 'int24', 'int24In32', 'int32', 'float32']
const FOLLOW = 'follow'
const FIXED = 'fixed'
const DEFAULT_SAMPLE_FORMAT: SampleFormat = 'int32'
const SLOT_CHANNELS = 2

const mode = computed({
  get: () => (settings.value.format === null ? FOLLOW : FIXED),
  set: (choice: string) => {
    const engineRate = store.state?.engine.sampleRate ?? RATES[1]
    const format: FormatView | null =
      choice === FOLLOW
        ? null
        : {
            sampleRate: RATES.includes(engineRate) ? engineRate : RATES[RATES.length - 1],
            sampleFormat: DEFAULT_SAMPLE_FORMAT,
            channels: SLOT_CHANNELS,
          }
    settings.value = { ...settings.value, format }
  },
})

function updateFormat(change: Partial<FormatView>): void {
  if (settings.value.format !== null) {
    settings.value = { ...settings.value, format: { ...settings.value.format, ...change } }
  }
}

const sampleRate = computed({
  get: () => settings.value.format?.sampleRate ?? 0,
  set: (value: number) => updateFormat({ sampleRate: value }),
})
const sampleFormat = computed({
  get: () => settings.value.format?.sampleFormat ?? DEFAULT_SAMPLE_FORMAT,
  set: (value: SampleFormat) => updateFormat({ sampleFormat: value }),
})
const resampler = computed({
  get: () => settings.value.resampler,
  set: (value: Resampler | null) => {
    settings.value = { ...settings.value, resampler: value }
  },
})
</script>

<template>
  <div class="slot-fields">
    <label class="field">
      <span>{{ t('slot.format') }}</span>
      <select v-model="mode" class="select">
        <button><selectedcontent></selectedcontent></button>
        <option :value="FOLLOW">{{ t('slot.follow') }}</option>
        <option :value="FIXED">{{ t('slot.fixed') }}</option>
      </select>
      <span v-if="mode === FOLLOW" class="caption">{{ t('slot.followHint') }}</span>
    </label>
    <div v-if="mode === FIXED" class="row">
      <label class="field">
        <span>{{ t('slot.rate') }}</span>
        <select v-model.number="sampleRate" class="select">
          <button><selectedcontent></selectedcontent></button>
          <option v-for="rate in RATES" :key="rate" :value="rate">
            {{ t('chip.rate', { rate: formatKhz(rate) }) }}
          </option>
        </select>
      </label>
      <label class="field">
        <span>{{ t('slot.sampleFormat') }}</span>
        <select v-model="sampleFormat" class="select">
          <button><selectedcontent></selectedcontent></button>
          <option v-for="format in SAMPLE_FORMATS" :key="format" :value="format">
            {{ t(`sampleFormat.${format}`) }}
          </option>
        </select>
      </label>
    </div>
    <ResamplerField v-model="resampler" />
  </div>
</template>

<style scoped>
.slot-fields {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.row {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  gap: 12px;
}
</style>
