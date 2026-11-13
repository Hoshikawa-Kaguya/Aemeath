<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import { open } from '@tauri-apps/plugin-dialog'
import type { Appearance, LanguagePreference, Resampler, ThemeMode } from '@/api/types'
import AppIcon from '@/components/AppIcon.vue'
import { formatKhz } from '@/format/format'
import { availableLocales } from '@/i18n'
import { useAppStore } from '@/stores/app'
import ResamplerField from '@/workbench/ResamplerField.vue'

// 设置页：音频（引擎采样率、全局重采样算法、主时钟）、设备检测（立即重新检测全部设备；自动检测始终开启，不提供开关）、界面语言。
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

const globalResampler = computed<Resampler | null>({
  get: () => store.state?.engine.resampler ?? null,
  set: (resampler) => {
    if (resampler !== null) {
      void store.run(() => backend.setGlobalResampler(resampler))
    }
  },
})

/** 可以当主时钟的节点：外部输入和外部输出 */
const clockChoices = computed(() =>
  store.nodes.filter((node) => node.kind === 'externalInput' || node.kind === 'externalOutput'),
)

function chooseClock(event: Event): void {
  const value = (event.target as HTMLSelectElement).value
  void store.run(() => backend.setClockMaster(value === SYSTEM ? null : Number(value)))
}

const update = computed(() => store.update)
const recording = computed(() => store.nodes.some((node) => node.recorder?.state.state === 'recording'))
const updateText = computed(() => {
  const current = update.value
  switch (current?.state) {
    case 'checking':
      return t('set.updateChecking')
    case 'upToDate':
      return t('set.updateLatest')
    case 'available':
      return t('set.updateAvailable', { version: current.version })
    case 'downloading':
      return t('set.updateDownloading', { percent: current.percent })
    case 'ready':
      return t('set.updateReady', { version: current.version })
    case 'failed':
      return t('set.updateFailed')
    default:
      return t('set.updateTitle')
  }
})

function runUpdate(command: () => Promise<void>): void {
  command().catch((error: unknown) => store.showToast(store.errorText(error)))
}

function openReleases(): void {
  runUpdate(backend.openReleasesPage)
}

async function applyUpdate(): Promise<void> {
  if (await store.confirm(t('set.updateConfirm'), t('set.updateApply'))) {
    runUpdate(backend.applyUpdate)
  }
}

function setCheckOnStart(event: Event): void {
  const enabled = (event.target as HTMLInputElement).checked
  void store.run(() => backend.setCheckUpdatesOnStart(enabled))
}

const ACCENT_COLORS = ['#0f6cbd', '#8764b8', '#c239b3', '#d13438', '#ca5010', '#ffb900', '#107c10', '#038387']
const DEFAULT_PICKER_COLOR = '#0f6cbd'
const MAX_OVERLAY = 90
const IMAGE_EXTENSIONS = ['png', 'jpg', 'jpeg', 'webp']

const appearance = computed(() => store.state?.appearance)
// 滑块和取色器用本地值：直接绑快照的话，每次重新渲染都会把拖到一半的值写回旧值
const overlay = ref(0)
const pickerColor = ref(DEFAULT_PICKER_COLOR)
watch(
  () => appearance.value?.overlayPercent,
  (value) => (overlay.value = value ?? 0),
  { immediate: true },
)
watch(
  () => appearance.value?.accent,
  (value) => (pickerColor.value = value ?? DEFAULT_PICKER_COLOR),
  { immediate: true },
)

function updateAppearance(change: Partial<Omit<Appearance, 'backgroundPath'>>): void {
  const current = appearance.value
  if (current !== undefined) {
    const { themeMode, accent, overlayPercent } = current
    void store.run(() => backend.setAppearance({ themeMode, accent, overlayPercent, ...change }))
  }
}

function setThemeMode(event: Event): void {
  updateAppearance({ themeMode: (event.target as HTMLSelectElement).value as ThemeMode })
}

function setAccent(accent: string | null): void {
  updateAppearance({ accent })
}

function setOverlay(): void {
  updateAppearance({ overlayPercent: overlay.value })
}

async function chooseBackground(): Promise<void> {
  const path = await open({
    multiple: false,
    filters: [{ name: t('set.backgroundFilter'), extensions: IMAGE_EXTENSIONS }],
  })
  if (path !== null) {
    await store.run(() => backend.setBackgroundImage(path))
  }
}

const autostart = ref(false)

function setAutostart(event: Event): void {
  const input = event.target as HTMLInputElement
  backend
    .setAutostart(input.checked)
    .then((enabled) => (autostart.value = enabled))
    .catch((error: unknown) => {
      // 值没变时 Vue 不会更新 DOM，失败要手动把勾选状态改回去
      input.checked = autostart.value
      store.showToast(store.errorText(error))
    })
}

const crashDumps = ref(false)

onMounted(() => {
  backend
    .autostartEnabled()
    .then((enabled) => (autostart.value = enabled))
    .catch((error: unknown) => store.showToast(store.errorText(error)))
  backend
    .crashDumpsEnabled()
    .then((enabled) => (crashDumps.value = enabled))
    .catch((error: unknown) => store.showToast(store.errorText(error)))
})

function enableCrashDumps(): void {
  backend
    .enableCrashDumps()
    .then((enabled) => (crashDumps.value = enabled))
    .catch((error: unknown) => store.showToast(store.errorText(error)))
}

async function removeVirtualDevices(): Promise<void> {
  if (await store.confirm(t('set.removeConfirm'), t('set.removeAction'))) {
    runUpdate(backend.removeVirtualDevices)
  }
}

function openFolder(open: () => Promise<void>): void {
  open().catch((error: unknown) => store.showToast(store.errorText(error)))
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
        <h2>{{ t('set.audio') }}</h2>
        <div class="set-row">
          <AppIcon name="sync" />
          <div class="text">
            <span>{{ t('set.engineRate') }}</span>
            <span class="caption">{{ t('set.engineRateSub') }}</span>
          </div>
          <fluent-button @click="store.rateDialogOpen = true">
            {{
              store.rateSwitching
                ? t('chip.rateSwitching')
                : t('chip.rate', { rate: formatKhz(store.state?.engine.sampleRate ?? 0) })
            }}
          </fluent-button>
        </div>
        <div class="set-row">
          <AppIcon name="sync" />
          <div class="text">
            <span>{{ t('set.resampler') }}</span>
            <span class="caption">{{ t('set.resamplerSub') }}</span>
          </div>
          <ResamplerField v-model="globalResampler" class="resampler" :allow-follow="false" />
        </div>
        <div class="set-row">
          <AppIcon name="clock" />
          <div class="text">
            <span>{{ t('set.clock') }}</span>
            <span class="caption">{{ t('set.clockSub') }}</span>
          </div>
          <select
            class="select"
            :value="store.state?.engine.clockMaster ?? SYSTEM"
            :aria-label="t('set.clock')"
            @change="chooseClock"
          >
            <button><selectedcontent></selectedcontent></button>
            <option :value="SYSTEM">{{ t('set.clockSystem') }}</option>
            <option v-for="node in clockChoices" :key="node.id" :value="node.id">{{ node.name }}</option>
          </select>
        </div>
      </div>
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
            <button><selectedcontent></selectedcontent></button>
            <option :value="SYSTEM">{{ t('set.langSys') }}</option>
            <option v-for="locale in availableLocales" :key="locale" :value="locale">
              {{ t('set.langName', {}, { locale }) }}
            </option>
          </select>
        </div>
        <div class="set-row">
          <AppIcon name="sync" />
          <div class="text">
            <span>{{ t('set.themeMode') }}</span>
            <span class="caption">{{ t('set.themeModeSub') }}</span>
          </div>
          <select class="select" :value="appearance?.themeMode" :aria-label="t('set.themeMode')" @change="setThemeMode">
            <button><selectedcontent></selectedcontent></button>
            <option value="system">{{ t('set.themeSystem') }}</option>
            <option value="dark">{{ t('set.themeDark') }}</option>
            <option value="light">{{ t('set.themeLight') }}</option>
          </select>
        </div>
        <div class="set-row">
          <AppIcon name="sync" />
          <div class="text">
            <span>{{ t('set.accent') }}</span>
            <span class="caption">{{ t('set.accentSub') }}</span>
          </div>
          <div class="buttons">
            <button
              v-for="color in ACCENT_COLORS"
              :key="color"
              type="button"
              class="swatch"
              :class="{ selected: appearance?.accent === color }"
              :style="{ background: color }"
              :aria-label="color"
              @click="setAccent(color)"
            ></button>
            <input
              v-model="pickerColor"
              class="color"
              type="color"
              :aria-label="t('set.accentCustom')"
              @change="setAccent(pickerColor)"
            />
            <fluent-button :disabled="appearance?.accent === null" @click="setAccent(null)">
              {{ t('set.accentReset') }}
            </fluent-button>
          </div>
        </div>
        <div class="set-row">
          <AppIcon name="sync" />
          <div class="text">
            <span>{{ t('set.background') }}</span>
            <span class="caption">{{ t('set.backgroundSub') }}</span>
          </div>
          <div class="buttons">
            <fluent-button @click="chooseBackground">{{ t('set.backgroundChoose') }}</fluent-button>
            <fluent-button
              :disabled="!appearance?.backgroundPath"
              @click="store.run(() => backend.removeBackgroundImage())"
            >
              {{ t('set.backgroundRemove') }}
            </fluent-button>
          </div>
        </div>
        <div class="set-row">
          <AppIcon name="sync" />
          <div class="text">
            <span>{{ t('set.overlay') }}</span>
            <span class="caption">{{ t('set.overlaySub') }}</span>
          </div>
          <input
            v-model.number="overlay"
            class="slider overlay-slider"
            type="range"
            min="0"
            :max="MAX_OVERLAY"
            :aria-label="t('set.overlay')"
            @change="setOverlay"
          />
        </div>
      </div>
      <div class="set-group">
        <h2>{{ t('set.startup') }}</h2>
        <label class="set-row">
          <AppIcon name="info" />
          <span class="text">
            <span>{{ t('set.autostart') }}</span>
            <span class="caption">{{ t('set.autostartSub') }}</span>
          </span>
          <span class="switch">
            <input type="checkbox" role="switch" :checked="autostart" @change="setAutostart" />
            <span class="track" aria-hidden="true"><span class="thumb"></span></span>
          </span>
        </label>
      </div>
      <div v-if="update?.supported" class="set-group">
        <h2>{{ t('set.update') }}</h2>
        <div class="set-row">
          <AppIcon name="sync" />
          <div class="text">
            <span>{{ updateText }}</span>
            <span class="caption">{{ t('set.updateCurrent', { version: update.currentVersion }) }}</span>
            <span v-if="update.state === 'ready' && recording" class="caption warning">{{
              t('set.updateRecording')
            }}</span>
          </div>
          <div class="buttons">
            <fluent-button v-if="update.state === 'available' || update.state === 'failed'" @click="openReleases">
              {{ t(update.state === 'available' ? 'set.updateNotes' : 'set.updateReleases') }}
            </fluent-button>
            <fluent-button
              v-if="update.state === 'available'"
              appearance="primary"
              @click="runUpdate(backend.downloadUpdate)"
            >
              {{ t('set.updateDownload') }}
            </fluent-button>
            <fluent-button
              v-else-if="update.state === 'ready'"
              appearance="primary"
              :disabled="recording"
              @click="applyUpdate"
            >
              {{ t('set.updateApply') }}
            </fluent-button>
            <fluent-button
              v-else
              :disabled="update.state === 'checking' || update.state === 'downloading'"
              @click="runUpdate(backend.checkUpdates)"
            >
              {{ t('set.updateCheck') }}
            </fluent-button>
          </div>
        </div>
        <label class="set-row">
          <AppIcon name="info" />
          <span class="text">{{ t('set.updateOnStart') }}</span>
          <span class="switch">
            <input
              type="checkbox"
              role="switch"
              :checked="store.state?.checkUpdatesOnStart"
              @change="setCheckOnStart"
            />
            <span class="track" aria-hidden="true"><span class="thumb"></span></span>
          </span>
        </label>
      </div>
      <div class="set-group">
        <h2>{{ t('set.data') }}</h2>
        <div class="set-row">
          <AppIcon name="info" />
          <div class="text">
            <span>{{ t('set.folders') }}</span>
            <span class="caption">{{ t('set.foldersSub') }}</span>
          </div>
          <div class="buttons">
            <fluent-button @click="openFolder(backend.openDataFolder)">{{ t('set.openData') }}</fluent-button>
            <fluent-button @click="openFolder(backend.openLogFolder)">{{ t('set.openLogs') }}</fluent-button>
          </div>
        </div>
        <div class="set-row">
          <AppIcon name="warn" />
          <div class="text">
            <span>{{ t('set.crash') }}</span>
            <span class="caption">{{ t('set.crashSub') }}</span>
          </div>
          <span v-if="crashDumps" class="caption">{{ t('set.crashEnabled') }}</span>
          <fluent-button v-else @click="enableCrashDumps">{{ t('set.crashEnable') }}</fluent-button>
        </div>
        <div v-if="update?.portable" class="set-row">
          <AppIcon name="unplug" />
          <div class="text">
            <span>{{ t('set.removeDevices') }}</span>
            <span class="caption">{{ t('set.removeDevicesSub') }}</span>
          </div>
          <fluent-button @click="removeVirtualDevices">{{ t('set.removeAction') }}</fluent-button>
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
  margin: 0 0 6px;
  font: var(--fontWeightSemibold) var(--fontSizeBase400) / var(--lineHeightBase400) var(--fontFamilyBase);
}

.set-row {
  display: grid;
  grid-template-columns: 32px minmax(0, 1fr) auto;
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
}

/* 右侧的下拉框（含重采样算法那一组）统一宽度 */
.resampler,
.select {
  width: 280px;
}

.buttons {
  display: flex;
  gap: 8px;
}

.warning {
  color: var(--warn);
}

.buttons {
  align-items: center;
}

.swatch {
  width: 24px;
  height: 24px;
  padding: 0;
  border: 2px solid transparent;
  border-radius: 50%;
  cursor: pointer;
}

.swatch.selected {
  border-color: var(--text);
}

.color {
  width: 32px;
  height: 28px;
  padding: 0;
  border: 1px solid var(--stroke-strong);
  border-radius: var(--r-ctl);
  background: transparent;
}

.overlay-slider {
  width: 200px;
}
</style>
