<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, ref, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import AppIcon, { type IconName } from '@/components/AppIcon.vue'
import { useAppStore, type Page } from '@/stores/app'

// 导航：工作台、设备、应用、录音、热键、工具；"设置"固定在最底部。
// 选中指示条只有一个，切换页面时滑到新的选中项（WinUI NavigationView 的效果）。
const { t } = useI18n()
const store = useAppStore()
const rail = useTemplateRef<HTMLElement>('rail')

const top: { page: Page; icon: IconName }[] = [
  { page: 'workbench', icon: 'graph' },
  { page: 'devices', icon: 'slots' },
  { page: 'apps', icon: 'apps' },
  { page: 'recordings', icon: 'rec' },
  { page: 'hotkeys', icon: 'key' },
  { page: 'tools', icon: 'tools' },
]

/** 指示条与导航项上下边的距离 */
const INDICATOR_INSET = 10
const indicator = ref<{ top: number; height: number } | null>(null)

/** 把选中指示条移到当前页面对应的导航项旁边；位置按导航项的实际布局计算 */
function placeIndicator(): void {
  const current = rail.value?.querySelector<HTMLElement>('.nav-item[aria-current="page"]')
  indicator.value =
    current === null || current === undefined
      ? null
      : { top: current.offsetTop + INDICATOR_INSET, height: current.offsetHeight - INDICATOR_INSET * 2 }
}

watch(
  () => store.page,
  () => void nextTick(placeIndicator),
)
onMounted(() => {
  placeIndicator()
  window.addEventListener('resize', placeIndicator)
})
onBeforeUnmount(() => window.removeEventListener('resize', placeIndicator))
</script>

<template>
  <nav ref="rail" class="rail" :aria-label="t('nav.aria')">
    <span
      v-if="indicator"
      class="indicator"
      aria-hidden="true"
      :style="{ transform: `translateY(${indicator.top}px)`, height: `${indicator.height}px` }"
    ></span>
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
  position: relative;
  flex: 1;
  min-height: 0;
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

/* 唯一的选中指示条：在导航项之间滑动 */
.indicator {
  position: absolute;
  top: 0;
  left: 8px;
  z-index: 1;
  width: 3px;
  border-radius: 2px;
  background: var(--accent);
  pointer-events: none;
  transition: transform var(--dur-slow) var(--ease-move);
}

/* 设置固定在导航栏最底部 */
.nav-sep {
  height: 1px;
  background: var(--stroke);
  margin: auto 4px 8px;
}
</style>
