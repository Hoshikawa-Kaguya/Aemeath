<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import AppIcon from '@/components/AppIcon.vue'
import { useAppStore } from '@/stores/app'

// 删除后的"撤销"提示，以及命令失败的原因
const { t } = useI18n()
const store = useAppStore()
</script>

<template>
  <div v-if="store.toast" class="toast" role="status" aria-live="polite">
    <span class="message">{{ store.toast.message }}</span>
    <fluent-button v-if="store.toast.undoable" size="small" @click="store.undo()">{{ t('toast.undo') }}</fluent-button>
    <button type="button" class="close" :aria-label="t('toast.dismiss')" @click="store.hideToast()">
      <AppIcon name="x" />
    </button>
  </div>
</template>

<style scoped>
.toast {
  position: absolute;
  right: 16px;
  bottom: 16px;
  z-index: 10;
  display: flex;
  align-items: center;
  gap: 12px;
  max-width: 520px;
  padding: 8px 8px 8px 12px;
  border-radius: var(--r-card);
  background: var(--card-solid);
  border: 1px solid var(--stroke-strong);
  box-shadow: var(--shadow);
  font-size: var(--fontSizeBase200);
}

/* 提示里带节点名，可能很长：最多两行 */
.message {
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  overflow-wrap: anywhere;
}

.close {
  display: inline-flex;
  border: 0;
  background: transparent;
  color: var(--text-3);
  cursor: pointer;
  padding: 4px;
  border-radius: var(--r-ctl);
}

.close:hover {
  background: var(--card-hover);
}
</style>
