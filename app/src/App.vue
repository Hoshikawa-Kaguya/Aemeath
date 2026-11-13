<script setup lang="ts">
import { computed, onMounted } from 'vue'
import { convertFileSrc } from '@tauri-apps/api/core'
import { useI18n } from 'vue-i18n'
// 与 exe、托盘共用同一套图标文件，换图标时只改 src-tauri/icons
import logoUrl from '../src-tauri/icons/128x128.png'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import EngineRateDialog from '@/components/EngineRateDialog.vue'
import IconSprite from '@/components/IconSprite.vue'
import NavRail from '@/components/NavRail.vue'
import RegistrationPanel from '@/components/RegistrationPanel.vue'
import TitleBar from '@/components/TitleBar.vue'
import ToastBar from '@/components/ToastBar.vue'
import ContextMenu from '@/menu/ContextMenu.vue'
import AppsPage from '@/pages/AppsPage.vue'
import DevicesPage from '@/pages/DevicesPage.vue'
import ToolsPage from '@/pages/ToolsPage.vue'
import SettingsPage from '@/pages/SettingsPage.vue'
import RecordingsPage from '@/pages/RecordingsPage.vue'
import HotkeysPage from '@/pages/HotkeysPage.vue'
import WorkbenchPage from '@/pages/WorkbenchPage.vue'
import TipLayer from '@/tip/TipLayer.vue'
import { useAppStore } from '@/stores/app'

const { t } = useI18n()
const store = useAppStore()

/** 用户的背景图：铺满窗口，遮罩的颜色随深浅色变化 */
const appearance = computed(() => store.state?.appearance)
const backdrop = computed(() => {
  const path = appearance.value?.backgroundPath
  return path ? { backgroundImage: `url("${convertFileSrc(path)}")` } : null
})

onMounted(() => {
  store.start().catch((error: unknown) => store.showToast(store.errorText(error)))
})
</script>

<template>
  <IconSprite />
  <template v-if="backdrop">
    <div class="backdrop" :style="backdrop"></div>
    <div class="backdrop overlay" :style="{ opacity: (appearance?.overlayPercent ?? 0) / 100 }"></div>
  </template>
  <div class="window" :class="{ 'has-backdrop': backdrop }">
    <!-- 左列从窗口顶部开始，竖分隔线贯穿全高；右列上方是状态区 -->
    <aside class="side">
      <div class="brand" data-tauri-drag-region>
        <img :src="logoUrl" class="logo" alt="" />
        <b data-tauri-drag-region>{{ t('app.title') }}</b>
      </div>
      <NavRail />
    </aside>
    <div class="main">
      <TitleBar />
      <main class="content">
        <!-- 切换页面时的进入动画（WinUI 的页面进入：上移加淡入） -->
        <!-- 等第一份快照到了再显示，否则上次停留在别的页面时会先闪一下工作台 -->
        <Transition v-if="store.state !== null" name="page-enter" mode="out-in">
          <WorkbenchPage v-if="store.page === 'workbench'" />
          <DevicesPage v-else-if="store.page === 'devices'" />
          <AppsPage v-else-if="store.page === 'apps'" />
          <RecordingsPage v-else-if="store.page === 'recordings'" />
          <HotkeysPage v-else-if="store.page === 'hotkeys'" />
          <ToolsPage v-else-if="store.page === 'tools'" />
          <SettingsPage v-else />
        </Transition>
        <RegistrationPanel />
        <ToastBar />
      </main>
    </div>
  </div>
  <ContextMenu />
  <ConfirmDialog />
  <EngineRateDialog />
  <TipLayer />
</template>

<style scoped>
.backdrop {
  position: fixed;
  inset: 0;
  z-index: -1;
  background-size: cover;
  background-position: center;
  pointer-events: none;
}

.overlay {
  background: var(--window-bg);
}

.window {
  display: grid;
  grid-template-columns: 216px minmax(0, 1fr);
  height: 100vh;
}

.side {
  display: flex;
  flex-direction: column;
  min-height: 0;
  border-right: 1px solid var(--stroke);
}

.brand {
  display: flex;
  flex: none;
  align-items: center;
  gap: 10px;
  height: 48px;
  padding-left: 16px;
  font-size: var(--fontSizeBase200);
}

.brand b {
  font-weight: var(--fontWeightSemibold);
  font-size: var(--fontSizeBase300);
}

.logo {
  width: 20px;
  height: 20px;
  border-radius: var(--borderRadiusSmall);
}

.main {
  display: grid;
  grid-template-rows: 48px minmax(0, 1fr);
  min-width: 0;
  min-height: 0;
}

.content {
  position: relative;
  min-width: 0;
  min-height: 0;
  background: var(--layer);
  border-top: 1px solid var(--stroke);
  overflow: hidden;
}

/* 有背景图时图片的明暗只由遮罩决定，主区域再叠一层底色会让它几乎看不见 */
.has-backdrop .content {
  background: transparent;
}

.page-enter-enter-active {
  transition:
    opacity var(--dur-slow) var(--ease-enter),
    transform var(--dur-slow) var(--ease-enter);
}

.page-enter-leave-active {
  transition: opacity var(--dur-fast) var(--ease-exit);
}

.page-enter-enter-from {
  opacity: 0;
  transform: translateY(12px);
}

.page-enter-leave-to {
  opacity: 0;
}
</style>
