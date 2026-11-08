<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import AppIcon, { type IconName } from '@/components/AppIcon.vue'
import { useAppStore, type Page } from '@/stores/app'

// M1 只有工作台、设备、设置三页；应用、录音、热键在后续里程碑加入（开发计划 M2–M3）
const { t } = useI18n()
const store = useAppStore()

const top: { page: Page; icon: IconName }[] = [
  { page: 'workbench', icon: 'graph' },
  { page: 'devices', icon: 'slots' },
]
</script>

<template>
  <nav class="rail" :aria-label="t('nav.aria')">
    <button
      v-for="item in top"
      :key="item.page"
      type="button"
      class="nav-item"
      :aria-current="store.page === item.page ? 'page' : undefined"
      @click="store.page = item.page"
    >
      <AppIcon :name="item.icon" /><span class="ellipsis">{{ t(`nav.${item.page}`) }}</span>
    </button>
    <div class="nav-sep"></div>
    <button
      type="button"
      class="nav-item"
      :aria-current="store.page === 'settings' ? 'page' : undefined"
      @click="store.page = 'settings'"
    >
      <AppIcon name="gear" /><span class="ellipsis">{{ t('nav.settings') }}</span>
    </button>
  </nav>
</template>

<style scoped>
.rail {
  padding: 4px 8px 12px;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.nav-item {
  position: relative;
  display: flex;
  align-items: center;
  gap: 12px;
  height: 36px;
  padding: 0 12px;
  border-radius: var(--r-ctl);
  border: 0;
  background: transparent;
  cursor: pointer;
  color: var(--text);
  text-align: left;
  font: inherit;
}

.nav-item:hover {
  background: var(--card-hover);
}

.nav-item[aria-current='page'] {
  background: var(--layer-alt);
}

.nav-item[aria-current='page']::before {
  content: '';
  position: absolute;
  left: 0;
  top: 10px;
  bottom: 10px;
  width: 3px;
  border-radius: 2px;
  background: var(--accent);
}

/* 设置固定在导航栏最底部 */
.nav-sep {
  height: 1px;
  background: var(--stroke);
  margin: auto 4px 8px;
}
</style>
