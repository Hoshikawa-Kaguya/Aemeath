<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import AppIcon from '@/components/AppIcon.vue'
import { useAppStore } from '@/stores/app'

// 删除等操作的结果提示，以及命令失败的原因
const { t } = useI18n()
const store = useAppStore()
</script>

<template>
  <Transition name="toast">
    <div v-if="store.toast" class="toast" role="status" aria-live="polite">
      <span class="message">{{ store.toast.message }}</span>
      <button type="button" class="close" :aria-label="t('toast.dismiss')" @click="store.hideToast()">
        <AppIcon name="x" />
      </button>
    </div>
  </Transition>
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
  animation: toast-in var(--dur-slow) var(--ease-enter);
}

/* 收起时向下淡出，比滑入快；期间不再响应点击 */
.toast-leave-active {
  pointer-events: none;
  transition:
    opacity var(--dur-fast) var(--ease-exit),
    transform var(--dur-fast) var(--ease-exit);
}

.toast-leave-to {
  opacity: 0;
  transform: translateY(16px);
}

/* 提示条从下方滑入 */
@keyframes toast-in {
  from {
    opacity: 0;
    transform: translateY(16px);
  }
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
