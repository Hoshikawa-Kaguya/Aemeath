<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { useI18n } from 'vue-i18n'
import AppIcon from '@/components/AppIcon.vue'
import { useAppStore } from '@/stores/app'
import { formatKhz } from '@/format/format'

// 窗口没有系统标题栏（ADR-0013）：拖动区域用 data-tauri-drag-region 标出（双击可最大化），
// 三个窗口按钮由前端绘制，图标取 Windows 11 自带的 Segoe Fluent Icons 字体
const GLYPH_MINIMIZE = ''
const GLYPH_MAXIMIZE = ''
const GLYPH_RESTORE = ''
const GLYPH_CLOSE = ''

const { t } = useI18n()
const store = useAppStore()
const appWindow = getCurrentWindow()
const isMaximized = ref(false)
let stopListening: (() => void) | undefined

const engine = computed(() => store.state?.engine.state ?? 'connecting')
const engineText = computed(() => {
  switch (engine.value) {
    case 'running':
      return t('chip.engineRunning')
    case 'connecting':
      return t('chip.engineConnecting')
    case 'unresponsive':
      return t('chip.engineUnresponsive')
    case 'versionMismatch':
      return t('chip.engineVersionMismatch')
  }
  return ''
})
const offline = computed(() => store.nodes.filter((node) => node.status.state === 'offline').length)
const slotsUsed = computed(() => store.nodes.filter((node) => node.slot !== null).length)
const slotsTip = computed(() => {
  const version = store.state?.usbipVersion
  return version ? t('chip.slotsTip', { version }) : t('chip.slotsTipNoDriver')
})
const rate = computed(() => t('chip.rate', { rate: formatKhz(store.state?.engine.sampleRate ?? 0) }))

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
    <div class="brand" data-tauri-drag-region>
      <AppIcon name="logo" class="logo" />
      <b data-tauri-drag-region>{{ t('app.title') }}</b>
    </div>
    <div class="chips" data-tauri-drag-region :aria-label="t('titleBar.statusAria')">
      <span class="chip" :class="{ warn: engine !== 'running' }">
        <span class="dot" :class="engine" aria-hidden="true"></span>{{ engineText }}
      </span>
      <span v-if="offline > 0" class="chip warn"><AppIcon name="warn" />{{ t('chip.offline', { n: offline }) }}</span>
      <button type="button" class="chip chip-btn" :title="slotsTip" @click="store.page = 'devices'">
        <span class="dot" :class="store.state?.usbipVersion ? 'running' : 'unresponsive'" aria-hidden="true"></span>
        <AppIcon name="slots" />{{ t('chip.slots', { used: slotsUsed }) }}
      </button>
      <span class="chip">{{ rate }}</span>
    </div>
    <div class="spacer" data-tauri-drag-region></div>
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
  gap: 12px;
  height: 48px;
  flex: none;
  padding-left: 16px;
}

.brand {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: var(--fontSizeBase200);
}

.brand b {
  font-weight: var(--fontWeightSemibold);
  font-size: var(--fontSizeBase300);
}

.logo {
  width: 20px;
  height: 20px;
  color: var(--accent-text);
}

/* 窗口变窄时整块隐藏放不下的标签，不截断文字 */
.chips {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  height: 24px;
  overflow: hidden;
  min-width: 0;
  margin-left: 12px;
}

.chip {
  flex: none;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 24px;
  padding: 0 10px;
  border-radius: var(--borderRadiusCircular);
  background: var(--layer-alt);
  border: 1px solid var(--stroke);
  font-size: var(--fontSizeBase200);
  color: var(--text-2);
  white-space: nowrap;
}

.chip svg {
  width: 14px;
  height: 14px;
}

.chip.warn {
  color: var(--text);
  border-color: color-mix(in srgb, var(--warn) 45%, transparent);
  background: color-mix(in srgb, var(--warn) 12%, transparent);
}

.chip.warn svg {
  color: var(--warn);
}

.chip-btn {
  cursor: pointer;
  font-family: inherit;
}

.chip-btn:hover {
  background: var(--card-hover);
  color: var(--text);
}

.dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--warn);
}

.dot.running {
  background: var(--ok);
}

.dot.unresponsive,
.dot.versionMismatch {
  background: var(--crit);
}

.spacer {
  flex: 1;
  height: 100%;
}

.caption-buttons {
  display: flex;
  height: 100%;
  align-self: flex-start;
}

.caption-buttons button {
  width: 46px;
  height: 32px;
  border: none;
  background: transparent;
  color: var(--text);
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
