<script setup lang="ts">
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import type { DefaultDeviceView, DefaultDevicesView } from '@/api/types'
import AppIcon from '@/components/AppIcon.vue'
import { useAppStore } from '@/stores/app'
import { usePolling } from './usePolling'

// 工具页（GLOSSARY：工具页）：汇集辅助小功能，只改 Windows 的设置，不改工作台。
// M2 只有"系统默认设备"：分播放和录音两个方向，每个方向的"默认设备"和"默认通信设备"，选中后立即生效；
// 不记录、不还原。被 Aemeath 独占的设备不可选（其他程序打不开它）。
const REFRESH_MS = 3000

type Flow = 'render' | 'capture'
type Role = 'default' | 'communications'

const { t } = useI18n()
const store = useAppStore()
const devices = ref<DefaultDevicesView | null>(null)
const failed = ref(false)

async function refresh(): Promise<void> {
  try {
    devices.value = await backend.listDefaultDevices()
    failed.value = false
  } catch {
    failed.value = true
  }
}

usePolling(refresh, REFRESH_MS)

function current(list: DefaultDeviceView[], role: Role): string {
  const found = list.find((device) => (role === 'default' ? device.isDefault : device.isCommunications))
  return found?.endpointId ?? ''
}

function label(device: DefaultDeviceView): string {
  return device.exclusiveByAemeath ? t('tools.exclusive', { name: device.name }) : device.name
}

async function choose(role: Role, event: Event): Promise<void> {
  const endpointId = (event.target as HTMLSelectElement).value
  if (endpointId === '') {
    return
  }
  try {
    await backend.setDefaultDevice(endpointId, role === 'communications')
  } catch (error) {
    store.showToast(store.errorText(error))
  }
  await refresh()
}

const FLOWS: Flow[] = ['render', 'capture']
const ROLES: Role[] = ['default', 'communications']
</script>

<template>
  <section class="page">
    <header>
      <h1 class="page-title">{{ t('nav.tools') }}</h1>
      <p class="page-sub">{{ t('tools.sub') }}</p>
    </header>
    <div class="body">
      <div class="set-group">
        <h2>{{ t('tools.defaults') }}</h2>
        <p class="caption">{{ t('tools.defaultsSub') }}</p>
        <p v-if="failed" class="caption">{{ t('tools.unavailable') }}</p>
        <div v-for="flow in FLOWS" :key="flow" class="set-row">
          <AppIcon :name="flow === 'render' ? 'spk' : 'mic'" />
          <div class="text">
            <span>{{ t(`tools.${flow}`) }}</span>
          </div>
          <div class="pickers">
            <label v-for="role in ROLES" :key="role" class="field">
              <span class="caption">{{ t(`tools.${role}`) }}</span>
              <select
                class="select"
                :value="current(devices?.[flow] ?? [], role)"
                :disabled="devices === null"
                @change="choose(role, $event)"
              >
                <option v-if="current(devices?.[flow] ?? [], role) === ''" value="">{{ t('tools.none') }}</option>
                <option
                  v-for="device in devices?.[flow] ?? []"
                  :key="device.endpointId"
                  :value="device.endpointId"
                  :disabled="device.exclusiveByAemeath"
                >
                  {{ label(device) }}
                </option>
              </select>
            </label>
          </div>
        </div>
      </div>
    </div>
  </section>
</template>

<style scoped>
/* 整页一起滚动，滚动条占满内容区的全部高度 */
.page {
  height: 100%;
  overflow: auto;
}

.page-title {
  font: var(--fontWeightSemibold) var(--fontSizeHero700) / var(--lineHeightHero700) var(--fontFamilyBase);
  margin: 20px 24px 4px;
}

.page-sub {
  margin: 0 24px 8px;
  color: var(--text-2);
}

.body {
  padding: 8px 24px 32px;
}

.set-group {
  margin-top: 16px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.set-group > h2 {
  margin: 0;
  font: var(--fontWeightSemibold) var(--fontSizeBase400) / var(--lineHeightBase400) var(--fontFamilyBase);
}

.set-group > p {
  margin: 0 0 8px;
}

/* 列宽固定，长设备名在下拉框里截断 */
.set-row {
  display: grid;
  grid-template-columns: 32px minmax(0, 0.6fr) minmax(0, 2fr);
  gap: 14px;
  align-items: center;
  min-height: 64px;
  padding: 12px var(--card-padding-end) 12px 16px;
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
  min-width: 0;
}

.pickers {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
}
</style>
