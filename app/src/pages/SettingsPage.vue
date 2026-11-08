<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import type { LanguagePreference } from '@/api/types'
import AppIcon from '@/components/AppIcon.vue'
import { useAppStore } from '@/stores/app'

// 设置页：设备检测（立即重新检测全部设备；自动检测始终开启，不提供开关）、界面语言。
// 引擎采样率、主时钟、全局重采样算法等在后续里程碑加入。
const SYSTEM = 'system'

const { t } = useI18n()
const store = useAppStore()

const selected = computed(() => {
  const language = store.state?.language
  return language?.mode === 'fixed' ? language.locale : SYSTEM
})

function choose(event: Event): void {
  const value = (event.target as HTMLSelectElement).value
  const language: LanguagePreference = value === SYSTEM ? { mode: 'system' } : { mode: 'fixed', locale: value }
  void store.run(() => backend.setLanguage(language))
}

function redetectAll(): void {
  backend
    .redetect(null)
    .then(() => store.showToast(t('set.detectStarted')))
    .catch((error: unknown) => store.showToast(store.errorText(error)))
}
</script>

<template>
  <section class="page">
    <header>
      <h1 class="page-title">{{ t('nav.settings') }}</h1>
    </header>
    <div class="body">
      <div class="set-group">
        <h2>{{ t('set.detect') }}</h2>
        <div class="set-row">
          <AppIcon name="sync" />
          <div class="text">
            <span>{{ t('set.detectNow') }}</span>
            <span class="caption">{{ t('set.detectSub') }}</span>
          </div>
          <fluent-button @click="redetectAll">{{ t('set.detectButton') }}</fluent-button>
        </div>
      </div>
      <div class="set-group">
        <h2>{{ t('set.ui') }}</h2>
        <div class="set-row">
          <AppIcon name="lang" />
          <div class="text">
            <span>{{ t('set.lang') }}</span>
            <span class="caption">{{ t('set.langSub') }}</span>
          </div>
          <select class="select" :value="selected" :aria-label="t('set.lang')" @change="choose">
            <option :value="SYSTEM">{{ t('set.langSys') }}</option>
            <option value="zh-CN">{{ t('set.langZh') }}</option>
            <option value="en">{{ t('set.langEn') }}</option>
          </select>
        </div>
      </div>
    </div>
  </section>
</template>

<style scoped>
.page {
  display: grid;
  grid-template-rows: auto minmax(0, 1fr);
  height: 100%;
}

.page-title {
  font: var(--fontWeightSemibold) var(--fontSizeHero700) / var(--lineHeightHero700) var(--fontFamilyBase);
  margin: 20px 24px 4px;
}

.body {
  padding: 8px 24px 32px;
  overflow: auto;
  min-height: 0;
}

.set-group {
  max-width: 1000px;
  margin-top: 16px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.set-group > h2 {
  margin: 0 0 6px;
  font: var(--fontWeightSemibold) var(--fontSizeBase400) / var(--lineHeightBase400) var(--fontFamilyBase);
}

.set-row {
  display: grid;
  grid-template-columns: 32px minmax(0, 1fr) auto;
  gap: 14px;
  align-items: center;
  min-height: 64px;
  padding: 12px 16px;
  border-radius: var(--r-card);
  background: var(--card);
  border: 1px solid var(--stroke);
}

.set-row > svg {
  width: 20px;
  height: 20px;
  color: var(--text-2);
  justify-self: center;
}

.text {
  display: flex;
  flex-direction: column;
}
</style>
