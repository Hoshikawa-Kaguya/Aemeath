<script setup lang="ts">
import { onMounted } from 'vue'
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
import WorkbenchPage from '@/pages/WorkbenchPage.vue'
import { useAppStore } from '@/stores/app'

const store = useAppStore()

onMounted(() => {
  store.start().catch((error: unknown) => store.showToast(store.errorText(error)))
})
</script>

<template>
  <IconSprite />
  <div class="window">
    <TitleBar />
    <div class="shell">
      <NavRail />
      <!-- 内容区是叠在 Mica 上的一层半透明底色，标题栏和导航保持透明，让 Mica 直接透出来 -->
      <main class="content">
        <!-- 切换页面时的进入动画（WinUI 的页面进入：上移加淡入） -->
        <!-- 等第一份快照到了再显示，否则上次停留在别的页面时会先闪一下工作台 -->
        <Transition v-if="store.state !== null" name="page-enter" mode="out-in">
          <WorkbenchPage v-if="store.page === 'workbench'" />
          <DevicesPage v-else-if="store.page === 'devices'" />
          <AppsPage v-else-if="store.page === 'apps'" />
          <ToolsPage v-else-if="store.page === 'tools'" />
          <SettingsPage v-else />
        </Transition>
        <RegistrationPanel />
        <ToastBar />
      </main>
    </div>
  </div>
  <ContextMenu />
</template>

<style scoped>
.window {
  display: grid;
  grid-template-rows: 48px minmax(0, 1fr);
  height: 100vh;
}

.shell {
  display: grid;
  grid-template-columns: 216px minmax(0, 1fr);
  min-height: 0;
}

.content {
  position: relative;
  min-width: 0;
  min-height: 0;
  background: var(--layer);
  border-top-left-radius: var(--r-card);
  border: 1px solid var(--stroke);
  border-right: 0;
  border-bottom: 0;
  overflow: hidden;
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
