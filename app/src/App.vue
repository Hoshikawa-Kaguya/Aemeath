<script setup lang="ts">
import { onMounted } from 'vue'
import IconSprite from '@/components/IconSprite.vue'
import NavRail from '@/components/NavRail.vue'
import RegistrationPanel from '@/components/RegistrationPanel.vue'
import TitleBar from '@/components/TitleBar.vue'
import ToastBar from '@/components/ToastBar.vue'
import DevicesPage from '@/pages/DevicesPage.vue'
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
        <WorkbenchPage v-if="store.page === 'workbench'" />
        <DevicesPage v-else-if="store.page === 'devices'" />
        <SettingsPage v-else />
        <RegistrationPanel />
        <ToastBar />
      </main>
    </div>
  </div>
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
</style>
