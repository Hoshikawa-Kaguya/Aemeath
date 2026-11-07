<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { useI18n } from 'vue-i18n'

// 窗口没有系统标题栏（ADR-0013）：拖动区域用 data-tauri-drag-region 标出（双击可最大化），
// 三个窗口按钮由前端绘制，图标取 Windows 11 自带的 Segoe Fluent Icons 字体
const GLYPH_MINIMIZE = ''
const GLYPH_MAXIMIZE = ''
const GLYPH_RESTORE = ''
const GLYPH_CLOSE = ''

const { t } = useI18n()
const appWindow = getCurrentWindow()
const isMaximized = ref(false)
let stopListening: (() => void) | undefined

async function refreshMaximized(): Promise<void> {
  isMaximized.value = await appWindow.isMaximized()
}

onMounted(async () => {
  await refreshMaximized()
  stopListening = await appWindow.onResized(refreshMaximized)
})

onUnmounted(() => {
  stopListening?.()
})
</script>

<template>
  <header class="title-bar" data-tauri-drag-region>
    <span class="title" data-tauri-drag-region>{{ t('app.title') }}</span>
    <div class="status" data-tauri-drag-region>
      <slot />
    </div>
    <div class="caption-buttons">
      <button
        type="button"
        :title="t('titleBar.minimize')"
        :aria-label="t('titleBar.minimize')"
        @click="appWindow.minimize()"
      >
        {{ GLYPH_MINIMIZE }}
      </button>
      <button
        type="button"
        :title="isMaximized ? t('titleBar.restore') : t('titleBar.maximize')"
        :aria-label="isMaximized ? t('titleBar.restore') : t('titleBar.maximize')"
        @click="appWindow.toggleMaximize()"
      >
        {{ isMaximized ? GLYPH_RESTORE : GLYPH_MAXIMIZE }}
      </button>
      <button
        type="button"
        class="close"
        :title="t('titleBar.close')"
        :aria-label="t('titleBar.close')"
        @click="appWindow.close()"
      >
        {{ GLYPH_CLOSE }}
      </button>
    </div>
  </header>
</template>

<style scoped>
.title-bar {
  display: flex;
  align-items: center;
  height: 40px;
  flex: none;
}

.title {
  padding: 0 16px;
  font-size: var(--fontSizeBase200);
  color: var(--colorNeutralForeground1);
  white-space: nowrap;
}

.status {
  display: flex;
  flex: 1;
  min-width: 0;
  height: 100%;
  align-items: center;
  gap: 8px;
}

.caption-buttons {
  display: flex;
  height: 100%;
}

.caption-buttons button {
  width: 46px;
  height: 100%;
  border: none;
  background: transparent;
  color: var(--colorNeutralForeground1);
  font-family: 'Segoe Fluent Icons', 'Segoe MDL2 Assets';
  font-size: 10px;
}

.caption-buttons button:hover {
  background: var(--colorSubtleBackgroundHover);
}

.caption-buttons button:active {
  background: var(--colorSubtleBackgroundPressed);
}

/* 关闭按钮的悬停色沿用 Windows 系统标题栏的约定（#C42B1C 配白色图标），不随主题变化 */
.caption-buttons button.close:hover {
  background: #c42b1c;
  color: #ffffff;
}

.caption-buttons button.close:active {
  background: #c83c31;
  color: #ffffff;
}
</style>
