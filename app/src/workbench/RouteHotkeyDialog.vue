<script setup lang="ts">
import { computed, ref, useTemplateRef } from 'vue'
import { useI18n } from 'vue-i18n'
import type { RouteView } from '@/api/types'
import { useAppStore } from '@/stores/app'
import { describeTarget } from './hotkeyLabels'
import HotkeyField from './HotkeyField.vue'

// 连线的专属热键：从连线和路由矩阵格子的右键菜单打开。
interface FluentDialog extends HTMLElement {
  show(): void
  hide(): void
}

const { t } = useI18n()
const store = useAppStore()
const dialog = useTemplateRef<FluentDialog>('dialog')
const pair = ref<{ from: number; to: number } | null>(null)

const route = computed(() => store.routes.find((own) => own.from === pair.value?.from && own.to === pair.value?.to))
const target = computed(() => (pair.value === null ? null : ({ type: 'routeToggle', ...pair.value } as const)))

function open(selected: RouteView): void {
  pair.value = { from: selected.from, to: selected.to }
  dialog.value?.show()
}

defineExpose({ open })
</script>

<template>
  <fluent-dialog ref="dialog" type="modal">
    <fluent-dialog-body>
      <h2 slot="title" class="title ellipsis">{{ target ? describeTarget(target, store.nodes) : '' }}</h2>
      <div class="form">
        <HotkeyField v-if="target" :target="target" :binding="route?.toggleHotkey ?? null" />
        <p class="caption">{{ t('hotkey.hint') }}</p>
      </div>
      <fluent-button slot="action" appearance="primary" @click="dialog?.hide()">{{
        t('nodeSettings.confirm')
      }}</fluent-button>
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
</style>
