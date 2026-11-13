<script setup lang="ts">
import { computed } from 'vue'
import { open } from '@tauri-apps/plugin-dialog'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import { FLAC_MAX_HZ, type NodeView, type RecordDepth, type RecordFormat, type RecorderSettingsView } from '@/api/types'
import AppIcon from '@/components/AppIcon.vue'
import { formatBytes, formatDuration } from '@/format/format'
import { useAppStore } from '@/stores/app'
import { fileNameOf, isRecording } from '@/stores/nodeLabels'

// 录音页：录音文件夹、新录音节点的默认格式，以及每个录音节点的状态与开始/停止。不做文件浏览和回放。
const { t } = useI18n()
const store = useAppStore()

const FORMATS: RecordFormat[] = ['flac', 'wav']
const DEPTHS: RecordDepth[] = ['int16', 'int24', 'float32']

const recording = computed(() => store.state?.recording ?? null)
const recorders = computed(() => store.nodes.filter((node) => node.recorder !== null))
const rate = computed(() => store.state?.engine.sampleRate ?? 1)

function setDefaults(change: Partial<RecorderSettingsView>): void {
  const current = recording.value?.defaults
  if (current === undefined) {
    return
  }
  const next = { ...current, ...change }
  // FLAC 不支持浮点
  void store.run(() =>
    backend.setRecordingDefaults(
      next.format === 'flac' && next.depth === 'float32' ? { ...next, depth: 'int24' } : next,
    ),
  )
}

async function chooseFolder(): Promise<void> {
  const folder = await open({ directory: true, multiple: false, defaultPath: recording.value?.folder })
  if (folder !== null) {
    await store.run(() => backend.setRecordingFolder(folder))
  }
}

function openFolder(): void {
  backend.openRecordingFolder().catch((error: unknown) => store.showToast(store.errorText(error)))
}

function stateText(node: NodeView): string {
  const state = node.recorder?.state
  switch (state?.state) {
    case 'recording':
      return t('rec.recording')
    case 'failed':
      return t(`rec.failed.${state.reason}`)
    default:
      return t('rec.stopped')
  }
}
</script>

<template>
  <section class="page">
    <header>
      <h1 class="page-title">{{ t('nav.recordings') }}</h1>
      <p class="page-sub">{{ t('rec.sub') }}</p>
    </header>
    <div class="body">
      <div v-if="recording" class="settings">
        <div class="field wide">
          <span>{{ t('rec.folder') }}</span>
          <div class="row">
            <span v-tip="recording.folder" class="folder ellipsis">{{ recording.folder }}</span>
            <fluent-button @click="chooseFolder">{{ t('rec.chooseFolder') }}</fluent-button>
            <fluent-button @click="openFolder">{{ t('rec.openFolder') }}</fluent-button>
          </div>
          <span class="caption">{{ t('rec.folderHint') }}</span>
        </div>
        <label class="field">
          <span>{{ t('rec.defaultFormat') }}</span>
          <select
            class="select"
            :value="recording.defaults.format"
            @change="setDefaults({ format: ($event.target as HTMLSelectElement).value as RecordFormat })"
          >
            <button><selectedcontent></selectedcontent></button>
            <option v-for="format in FORMATS" :key="format" :value="format">{{ t(`rec.format.${format}`) }}</option>
          </select>
        </label>
        <label class="field">
          <span>{{ t('rec.defaultDepth') }}</span>
          <select
            class="select"
            :value="recording.defaults.depth"
            @change="setDefaults({ depth: ($event.target as HTMLSelectElement).value as RecordDepth })"
          >
            <button><selectedcontent></selectedcontent></button>
            <option
              v-for="depth in DEPTHS"
              :key="depth"
              :value="depth"
              :disabled="recording.defaults.format === 'flac' && depth === 'float32'"
            >
              {{ t(`rec.depth.${depth}`) }}
            </option>
          </select>
        </label>
      </div>

      <p
        v-if="recording?.defaults.format === 'flac' && (store.state?.engine.sampleRate ?? 0) > FLAC_MAX_HZ"
        class="caption"
      >
        {{ t('rec.flacRateHint') }}
      </p>
      <p v-if="recorders.length === 0" class="caption">{{ t('rec.empty') }}</p>
      <div class="cardlist">
        <div v-for="node in recorders" :key="node.id" class="card-row">
          <button
            type="button"
            class="rec-button"
            :class="{ on: isRecording(node) }"
            :aria-pressed="isRecording(node)"
            :aria-label="t(isRecording(node) ? 'rec.stopAria' : 'rec.startAria', { name: node.name })"
            @click="store.toggleRecording(node)"
          >
            <AppIcon :name="isRecording(node) ? 'stop' : 'rec'" />
          </button>
          <b v-tip="node.name" class="ellipsis">{{ node.name }}</b>
          <span :class="{ bad: node.recorder?.state.state === 'failed' }">{{ stateText(node) }}</span>
          <template v-if="node.recorder?.state.state === 'recording'">
            <span class="time">{{ formatDuration(node.recorder.state.frames / rate) }}</span>
            <span v-tip="node.recorder.state.file" class="caption ellipsis">
              {{ fileNameOf(node.recorder.state.file) }} · {{ formatBytes(node.recorder.state.bytes) }}
            </span>
          </template>
          <template v-else>
            <span class="time">—</span>
            <span class="caption"></span>
          </template>
        </div>
      </div>
    </div>
  </section>
</template>

<style scoped>
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

.settings {
  display: grid;
  grid-template-columns: 220px 220px;
  gap: 12px;
  margin-bottom: 20px;
}

.wide {
  grid-column: 1 / -1;
}

.row {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}

.folder {
  flex: 1;
  min-width: 0;
}

.cardlist {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

/* 列宽固定，长名字、长文件名都截断 */
.card-row {
  display: grid;
  grid-template-columns: 36px minmax(0, 1fr) 150px 72px minmax(0, 1.4fr);
  align-items: center;
  gap: 12px;
  min-height: 56px;
  padding: 8px var(--card-padding-end) 8px 16px;
  border-radius: var(--r-card);
  background: var(--card);
  border: 1px solid var(--stroke);
}

.card-row b {
  font-weight: var(--fontWeightSemibold);
}

.time {
  font-variant-numeric: tabular-nums;
}

.bad {
  color: var(--crit);
}

.rec-button {
  display: grid;
  place-items: center;
  width: 32px;
  height: 32px;
  padding: 0;
  border-radius: 50%;
  border: 1px solid var(--stroke-strong);
  background: var(--layer-alt);
  color: var(--crit);
  cursor: pointer;
}

.rec-button:hover {
  background: var(--card-hover);
}

.rec-button.on {
  background: var(--crit);
  border-color: transparent;
  color: var(--colorNeutralForegroundInverted);
}

.rec-button svg {
  width: 16px;
  height: 16px;
}
</style>
