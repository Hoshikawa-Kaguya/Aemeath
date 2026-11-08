<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import type { BufferChoice, Capabilities, DeviceSettings, FormatView, ResamplerKind } from '@/api/types'
import { formatKhz } from '@/format/format'

// 外部设备节点的设备设置（ADR-0014），添加节点和节点设置两个对话框共用：
// 独占（默认勾选）、独占时的格式与缓冲、按节点覆盖的重采样算法。可选的格式和周期由引擎探测设备得到。
const props = defineProps<{ endpointId: string | null; direction: 'input' | 'output' }>()
const settings = defineModel<DeviceSettings>({ required: true })
const { t } = useI18n()

/** 后端允许的自定义缓冲范围（aemeath_protocol::units::BufferMs） */
const BUFFER_MIN_MS = 1
const BUFFER_MAX_MS = 100
const BUFFER_STEP_MS = 0.5
const RECOMMENDED = 'recommended'
const RESAMPLERS: ResamplerKind[] = ['sincHigh', 'sincStandard', 'polynomial', 'linear']
const FOLLOW_GLOBAL = 'global'

const capabilities = ref<Capabilities | null>(null)
const probing = ref(false)

watch(
  () => [props.endpointId, props.direction] as const,
  async ([endpointId, direction]) => {
    capabilities.value = null
    if (endpointId === null) {
      return
    }
    probing.value = true
    try {
      const result = await backend.probeDevice(endpointId, direction)
      // 探测期间换了设备：丢弃旧结果
      if (props.endpointId === endpointId) {
        capabilities.value = result
      }
    } catch {
      capabilities.value = null
    } finally {
      probing.value = false
    }
  },
  { immediate: true },
)

function formatKey(format: FormatView): string {
  return `${format.sampleRate}-${format.sampleFormat}-${format.channels}`
}

function formatLabel(format: FormatView): string {
  return t('device.format', { rate: formatKhz(format.sampleRate), format: t(`sampleFormat.${format.sampleFormat}`) })
}

/** 可选的格式：设备支持的全部格式；已选定的格式不在其中时（设备换了）也保留，让用户看到 */
const formatOptions = computed(() => {
  const list = [...(capabilities.value?.exclusiveFormats ?? [])]
  const chosen = settings.value.format
  if (chosen !== null && !list.some((format) => formatKey(format) === formatKey(chosen))) {
    list.unshift(chosen)
  }
  return list
})

const recommendedLabel = computed(() => {
  const recommended = capabilities.value?.recommended
  return recommended === null || recommended === undefined
    ? t('device.recommended')
    : t('device.recommendedFormat', { format: formatLabel(recommended) })
})

const formatValue = computed({
  get: () => (settings.value.format === null ? RECOMMENDED : formatKey(settings.value.format)),
  set: (key: string) => {
    const format =
      key === RECOMMENDED ? null : (formatOptions.value.find((option) => formatKey(option) === key) ?? null)
    settings.value = { ...settings.value, format }
  },
})

const bufferValue = computed({
  get: () => settings.value.buffer.choice,
  set: (choice: BufferChoice['choice']) => {
    const buffer: BufferChoice =
      choice === 'milliseconds' ? { choice, ms: capabilities.value?.defaultPeriodMs ?? 10 } : { choice }
    settings.value = { ...settings.value, buffer }
  },
})

const customMs = computed({
  get: () => (settings.value.buffer.choice === 'milliseconds' ? settings.value.buffer.ms : 0),
  set: (ms: number) => {
    if (Number.isFinite(ms)) {
      const clamped = Math.min(BUFFER_MAX_MS, Math.max(BUFFER_MIN_MS, ms))
      settings.value = { ...settings.value, buffer: { choice: 'milliseconds', ms: clamped } }
    }
  },
})

const resamplerValue = computed({
  get: () => settings.value.resampler ?? FOLLOW_GLOBAL,
  set: (value: string) => {
    settings.value = { ...settings.value, resampler: value === FOLLOW_GLOBAL ? null : (value as ResamplerKind) }
  },
})

function periodLabel(key: 'device.bufferDefault' | 'device.bufferMinimum', ms: number | undefined): string {
  return ms === undefined ? t(`${key}Unknown`) : t(key, { ms: Math.round(ms * 10) / 10 })
}

const sharedFormat = computed(() => capabilities.value?.sharedFormat ?? null)
</script>

<template>
  <div class="device-fields">
    <label class="check">
      <input
        type="checkbox"
        :checked="settings.exclusive"
        @change="settings = { ...settings, exclusive: ($event.target as HTMLInputElement).checked }"
      />
      <span>
        <b>{{ t('device.exclusive') }}</b>
        <span class="caption">{{ t('device.exclusiveHint') }}</span>
      </span>
    </label>

    <template v-if="settings.exclusive">
      <label class="field">
        <span>{{ t('device.formatLabel') }}</span>
        <select v-model="formatValue" class="select">
          <option :value="RECOMMENDED">{{ recommendedLabel }}</option>
          <option v-for="format in formatOptions" :key="formatKey(format)" :value="formatKey(format)">
            {{ formatLabel(format) }}
          </option>
        </select>
        <span v-if="probing" class="caption">{{ t('device.probing') }}</span>
        <span v-else-if="endpointId !== null && capabilities === null" class="caption">{{
          t('device.probeFailed')
        }}</span>
      </label>

      <div class="field">
        <label for="device-buffer">{{ t('device.buffer') }}</label>
        <div class="row">
          <select id="device-buffer" v-model="bufferValue" class="select">
            <option value="deviceDefault">
              {{ periodLabel('device.bufferDefault', capabilities?.defaultPeriodMs) }}
            </option>
            <option value="deviceMinimum">
              {{ periodLabel('device.bufferMinimum', capabilities?.minimumPeriodMs) }}
            </option>
            <option value="milliseconds">{{ t('device.bufferCustom') }}</option>
          </select>
          <input
            v-if="settings.buffer.choice === 'milliseconds'"
            v-model.number="customMs"
            class="text-input ms"
            type="number"
            :min="BUFFER_MIN_MS"
            :max="BUFFER_MAX_MS"
            :step="BUFFER_STEP_MS"
            :aria-label="t('device.bufferMs')"
          />
          <span v-if="settings.buffer.choice === 'milliseconds'" class="unit">ms</span>
        </div>
        <span class="caption">{{ t('device.bufferHint') }}</span>
      </div>
    </template>
    <p v-else class="caption shared">
      {{
        sharedFormat === null ? t('device.sharedHint') : t('device.sharedFormat', { format: formatLabel(sharedFormat) })
      }}
    </p>

    <label class="field">
      <span>{{ t('device.resampler') }}</span>
      <select v-model="resamplerValue" class="select">
        <option :value="FOLLOW_GLOBAL">{{ t('device.resamplerGlobal') }}</option>
        <option v-for="kind in RESAMPLERS" :key="kind" :value="kind">{{ t(`resampler.${kind}.name`) }}</option>
      </select>
      <span class="caption">
        {{ settings.resampler === null ? t('device.resamplerGlobalHint') : t(`resampler.${settings.resampler}.hint`) }}
      </span>
    </label>
  </div>
</template>

<style scoped>
.device-fields {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.check {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  gap: 10px;
  align-items: start;
}

.check input {
  width: 16px;
  height: 16px;
  margin: 2px 0 0;
  accent-color: var(--accent);
}

.check > span {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.check b {
  font-weight: var(--fontWeightSemibold);
}

.row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.row .select {
  flex: 1;
  min-width: 0;
}

.ms {
  width: 88px;
  flex: none;
}

.unit {
  color: var(--text-2);
}

.shared {
  margin: 0;
}
</style>
