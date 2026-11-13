<script setup lang="ts">
import { ref, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { isClosing, type FluentDialog } from '@/components/dialogDismiss'
import { useAppStore } from '@/stores/app'

// 全局的确认对话框：由 store.confirm 打开。
const { t } = useI18n()
const store = useAppStore()
const dialog = useTemplateRef<FluentDialog>('dialog')
// 关闭后保留上一次的内容，收起过渡期间文字和按钮不会先消失
const shown = ref(store.confirmation)

watch(
  () => store.confirmation,
  (current) => {
    if (current === null) {
      dialog.value?.hide()
    } else {
      shown.value = current
      dialog.value?.show()
    }
  },
)
</script>
<template>
  <fluent-dialog ref="dialog" type="modal" @toggle="isClosing($event) && store.answerConfirmation(false)">
    <fluent-dialog-body>
      <p class="message">{{ shown?.message }}</p>
      <fluent-button slot="action" @click="store.answerConfirmation(false)">{{ t('add.cancel') }}</fluent-button>
      <fluent-button slot="action" appearance="primary" @click="store.answerConfirmation(true)">
        {{ shown?.action }}
      </fluent-button>
    </fluent-dialog-body>
  </fluent-dialog>
</template>

<style scoped>
.message {
  margin: 0;
}
</style>
