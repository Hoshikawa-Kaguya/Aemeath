<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import AppIcon from '@/components/AppIcon.vue'
import { useAppStore } from '@/stores/app'
import { slotCode } from '@/stores/nodeLabels'

// 首次启动注册 16 个槽位时盖在内容区上（ADR-0009：第一次安装驱动可能要 1 分钟左右）
const { t } = useI18n()
const store = useAppStore()

const registration = computed(() => store.state?.registration ?? null)
const engineRunning = computed(() => store.state?.engine.state === 'running')
const failing = computed(() => registration.value?.failing.map(slotCode).join(', ') ?? '')
</script>

<template>
  <div v-if="registration" class="overlay">
    <section class="panel" role="status" aria-live="polite">
      <h2>{{ t('reg.title') }}</h2>
      <p>{{ t('reg.body') }}</p>
      <fluent-progress-bar
        :value="registration.completed"
        :max="registration.total"
        :aria-label="t('reg.title')"
      ></fluent-progress-bar>
      <p class="caption">
        {{
          engineRunning
            ? t('reg.progress', { done: registration.completed, total: registration.total })
            : t('reg.waitingEngine')
        }}
      </p>
      <p v-if="failing" class="failing"><AppIcon name="warn" />{{ t('reg.failing', { slots: failing }) }}</p>
    </section>
  </div>
</template>

<style scoped>
.overlay {
  position: absolute;
  inset: 0;
  z-index: 20;
  display: grid;
  place-items: center;
  background: color-mix(in srgb, var(--colorNeutralBackground2) 60%, transparent);
  backdrop-filter: blur(2px);
}

.panel {
  width: min(520px, calc(100% - 48px));
  padding: 20px 24px;
  border-radius: var(--r-card);
  background: var(--card-solid);
  border: 1px solid var(--stroke-strong);
  box-shadow: var(--shadow16);
}

h2 {
  margin: 0 0 8px;
  font: var(--fontWeightSemibold) var(--fontSizeBase500) / var(--lineHeightBase500) var(--fontFamilyBase);
}

p {
  margin: 0 0 12px;
  color: var(--text-2);
}

fluent-progress-bar {
  width: 100%;
  margin: 8px 0;
}

.failing {
  display: flex;
  gap: 6px;
  align-items: flex-start;
  color: var(--text);
}

.failing svg {
  color: var(--warn);
  margin-top: 2px;
}
</style>
