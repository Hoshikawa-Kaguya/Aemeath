<script setup lang="ts">
import { computed, ref, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { formatKhz } from '@/format/format'
import { SLOT_FORMAT_MAX_HZ, useAppStore } from '@/stores/app'

// 修改引擎采样率：标题栏的采样率按钮和设置页共用。
interface FluentDialog extends HTMLElement {
  show(): void
  hide(): void
}

const RATES = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000, 705_600, 768_000]

const { t } = useI18n()
const store = useAppStore()
const dialog = useTemplateRef<FluentDialog>('dialog')
const rate = ref(RATES[1])

const current = computed(() => store.state?.engine.sampleRate)
const recording = computed(() => store.nodes.some((node) => node.recorder?.state.state === 'recording'))

watch(
  () => store.rateDialogOpen,
  (open) => {
    if (open) {
      rate.value = current.value ?? rate.value
      dialog.value?.show()
    } else {
      dialog.value?.hide()
    }
  },
)

function apply(): void {
  store.rateDialogOpen = false
  if (rate.value !== current.value) {
    void store.setEngineRate(rate.value)
  }
}
</script>

<template>
  <fluent-dialog ref="dialog" type="modal" @dismiss="store.rateDialogOpen = false">
    <fluent-dialog-body>
      <h2 slot="title" class="title">{{ t('rate.title') }}</h2>
      <div class="form">
        <label class="field">
          <span>{{ t('rate.label') }}</span>
          <select v-model.number="rate" class="select">
            <button><selectedcontent></selectedcontent></button>
            <option v-for="choice in RATES" :key="choice" :value="choice">
              {{ t('chip.rate', { rate: formatKhz(choice) }) }}
            </option>
          </select>
        </label>
        <p v-if="rate > SLOT_FORMAT_MAX_HZ" class="caption warning">{{ t('rate.over384') }}</p>
        <p class="caption">{{ t('rate.body') }}</p>
        <p v-if="recording" class="caption warning">{{ t('rate.recording') }}</p>
      </div>
      <fluent-button slot="action" @click="store.rateDialogOpen = false">{{ t('add.cancel') }}</fluent-button>
      <fluent-button slot="action" appearance="primary" @click="apply">{{ t('rate.apply') }}</fluent-button>
    </fluent-dialog-body>
  </fluent-dialog>
</template>

<style scoped>
.title {
  margin: 0;
  font: var(--fontWeightSemibold) var(--fontSizeBase500) / var(--lineHeightBase500) var(--fontFamilyBase);
}

.form {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.form p {
  margin: 0;
}

.warning {
  color: var(--warn);
}
</style>
