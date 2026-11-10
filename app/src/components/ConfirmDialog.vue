<script setup lang="ts">
import { useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useAppStore } from '@/stores/app'

// 全局的确认对话框：由 store.confirm 打开。
interface FluentDialog extends HTMLElement {
  show(): void
  hide(): void
}

const { t } = useI18n()
const store = useAppStore()
const dialog = useTemplateRef<FluentDialog>('dialog')

watch(
  () => store.confirmation,
  (current) => (current === null ? dialog.value?.hide() : dialog.value?.show()),
)
</script>

<template>
  <fluent-dialog ref="dialog" type="modal">
    <fluent-dialog-body>
      <p class="message">{{ store.confirmation?.message }}</p>
      <fluent-button slot="action" @click="store.answerConfirmation(false)">{{ t('add.cancel') }}</fluent-button>
      <fluent-button slot="action" appearance="primary" @click="store.answerConfirmation(true)">
        {{ store.confirmation?.action }}
      </fluent-button>
    </fluent-dialog-body>
  </fluent-dialog>
</template>

<style scoped>
.message {
  margin: 0;
}
</style>
