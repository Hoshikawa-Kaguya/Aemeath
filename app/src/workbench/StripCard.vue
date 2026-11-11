<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import { roleOf, type NodeView } from '@/api/types'
import AppIcon from '@/components/AppIcon.vue'
import { formatDuration, formatGain, formatThreshold } from '@/format/format'
import { isMenuKey } from '@/menu/contextMenu'
import NodeMeter from '@/meters/NodeMeter.vue'
import { useAppStore } from '@/stores/app'
import { nodeCode } from '@/stores/nodeLabels'
import { GAIN_MAX_DB, GAIN_MIN_DB } from './context'
import { GATE_MAX_DB, GATE_OFF_POSITION, gateToPosition, positionToGate } from './gate'
import { throttled } from './throttle'

// 控制台上的一条竖排通道条（GLOSSARY：通道条）：节点名、LEVEL / GAIN / GATE 三列竖条（门限只有输入节点才有，没有时去掉一列、通道条变窄）、静音。
// 同一类节点宽度固定，长名字截断，悬停显示全名。拖动推子时先改本地值，再节流发给后端。列标识固定用英文，不进 i18n。
const props = defineProps<{ node: NodeView }>()
const emit = defineEmits<{ menu: [event: MouseEvent | KeyboardEvent] }>()
const { t } = useI18n()
const store = useAppStore()

/** 推子的步进（dB） */
const GAIN_STEP = 0.5

const isInput = computed(() => roleOf(props.node.kind) === 'in')
const isRecording = computed(() => props.node.recorder?.state.state === 'recording')
const recordedTime = computed(() => {
  const state = props.node.recorder?.state
  return state?.state === 'recording' ? formatDuration(state.frames / (store.state?.engine.sampleRate ?? 1)) : '—'
})
const gain = ref(props.node.gainDb)
const gatePosition = ref(gateToPosition(props.node.gateDb))
/** 正在拖动时不让后端推来的快照把推子拉回去 */
let adjusting = false

watch(
  () => props.node.gainDb,
  (value) => {
    if (!adjusting) {
      gain.value = value
    }
  },
)
watch(
  () => props.node.gateDb,
  (value) => {
    if (!adjusting) {
      gatePosition.value = gateToPosition(value)
    }
  },
)

const sendGain = throttled((value: number) => void store.run(() => backend.setNodeGain(props.node.id, value)))
const sendGate = throttled((value: number | null) => void store.run(() => backend.setNodeGate(props.node.id, value)))

/** 拖动推子：本地值先变，节流发给后端 */
function onGain(): void {
  adjusting = true
  sendGain(gain.value)
}

function onGate(): void {
  adjusting = true
  sendGate(positionToGate(gatePosition.value))
}

/** 松开推子或滑块，恢复接收后端推来的值 */
function done(): void {
  adjusting = false
}

/** 双击推子回到 0 dB */
function resetGain(): void {
  gain.value = 0
  void store.run(() => backend.setNodeGain(props.node.id, 0))
}

const gateText = computed(() => {
  const gate = positionToGate(gatePosition.value)
  return gate === null ? t('nodeSettings.gateOff') : formatThreshold(gate)
})
/** 竖条底部的读数：关时固定写 OFF，与列标识一样不进 i18n */
const gateReadout = computed(() => {
  const gate = positionToGate(gatePosition.value)
  return gate === null ? 'OFF' : formatGain(gate)
})

function toggleMute(): void {
  void store.run(() => backend.setNodeMuted(props.node.id, !props.node.muted))
}

function onKeyDown(event: KeyboardEvent): void {
  if (isMenuKey(event)) {
    emit('menu', event)
  }
}
</script>

<template>
  <div
    class="strip"
    :class="{ muted: node.muted, 'no-gate': !isInput }"
    :data-role="roleOf(node.kind)"
    role="group"
    :aria-label="t('strip.aria', { name: node.name })"
    @contextmenu="emit('menu', $event)"
    @keydown="onKeyDown"
  >
    <div class="strip-head" draggable="true" :title="t('strip.dragHint', { name: node.name })">
      <span class="code">{{ nodeCode(node, store.nodes) }}</span>
      <b class="ellipsis">{{ node.name }}</b>
      <button
        type="button"
        class="more"
        :aria-label="t('menu.aria', { name: node.name })"
        draggable="false"
        @click.stop="emit('menu', $event)"
      >
        <AppIcon name="more" />
      </button>
    </div>
    <div class="columns">
      <div class="col">
        <span class="tag">LEVEL</span>
        <NodeMeter class="level" :node="node.id" :muted="node.muted" orientation="vertical" />
      </div>
      <div class="col">
        <span class="tag">GAIN</span>
        <input
          v-model.number="gain"
          class="fader"
          type="range"
          :min="GAIN_MIN_DB"
          :max="GAIN_MAX_DB"
          :step="GAIN_STEP"
          :aria-label="t('strip.gainAria', { name: node.name })"
          :aria-valuetext="formatGain(gain)"
          @input="onGain"
          @change="done"
          @dblclick="resetGain"
        />
        <span class="readout">{{ formatGain(gain) }}</span>
      </div>
      <div v-if="isInput" class="col" :title="gateText">
        <span class="tag">GATE</span>
        <input
          v-model.number="gatePosition"
          class="fader"
          type="range"
          :min="GATE_OFF_POSITION"
          :max="GATE_MAX_DB"
          step="1"
          :aria-label="t('strip.gateAria', { name: node.name })"
          :aria-valuetext="gateText"
          @input="onGate"
          @change="done"
        />
        <span class="readout">{{ gateReadout }}</span>
      </div>
    </div>
    <div class="strip-foot">
      <div v-if="node.recorder" class="rec">
        <button
          type="button"
          class="sbtn record"
          :aria-pressed="isRecording"
          :aria-label="t(isRecording ? 'rec.stopAria' : 'rec.startAria', { name: node.name })"
          @click="store.toggleRecording(node)"
        >
          <AppIcon :name="isRecording ? 'stop' : 'rec'" />
        </button>
        <span class="readout">{{ recordedTime }}</span>
      </div>
      <button
        v-else
        type="button"
        class="sbtn mute"
        :aria-pressed="node.muted"
        :aria-label="t('strip.muteAria', { name: node.name })"
        @click="toggleMute"
      >
        {{ t('strip.mute') }}
      </button>
    </div>
  </div>
</template>

<style scoped>
/* 宽度固定：长名字不撑开通道条；没有门限的节点少一列 */
.strip {
  flex: none;
  width: 170px;
  display: grid;
  grid-template-rows: auto minmax(0, 1fr) auto;
  gap: 6px;
  padding: 8px;
  border-radius: var(--r-card);
  background: var(--card);
  border: 1px solid var(--stroke);
}

.strip.no-gate {
  width: 118px;
}

.strip-head {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
  cursor: grab;
}

.strip-head b {
  flex: 1;
  font: var(--fontWeightSemibold) var(--fontSizeBase200) / 16px var(--fontFamilyBase);
}

.code {
  flex: none;
  font-size: 10px;
  color: var(--text-3);
}

.more {
  display: grid;
  place-items: center;
  flex: none;
  width: 22px;
  height: 22px;
  padding: 0;
  border: 0;
  border-radius: var(--r-ctl);
  background: transparent;
  color: var(--text-3);
  cursor: default;
}

.more:hover,
.more:focus-visible {
  background: var(--card-hover);
  color: var(--text);
  outline: none;
}

.more svg {
  width: 14px;
  height: 14px;
}

.columns {
  display: grid;
  grid-auto-flow: column;
  grid-auto-columns: minmax(0, 1fr);
  gap: 2px;
  min-height: 0;
}

/* 三行：列标识、竖条、读数。电平表自带读数，占后两行 */
.col {
  display: grid;
  grid-template-rows: 14px minmax(0, 1fr) 16px;
  gap: 4px;
  justify-items: center;
  min-width: 0;
  min-height: 0;
}

.col .level {
  grid-row: 2 / 4;
}

.tag {
  font-size: 10px;
  line-height: 14px;
  letter-spacing: 0.04em;
  color: var(--text-3);
}

.fader {
  writing-mode: vertical-lr;
  direction: rtl;
  width: 28px;
  height: 100%;
  min-height: 0;
  accent-color: var(--accent);
  cursor: pointer;
}

.strip-foot {
  display: grid;
}

.readout {
  font-size: var(--fontSizeBase100);
  line-height: 16px;
  color: var(--text-2);
  white-space: nowrap;
  font-variant-numeric: tabular-nums;
}

.sbtn {
  height: 28px;
  padding: 0 4px;
  border-radius: var(--r-ctl);
  border: 1px solid var(--stroke-strong);
  background: var(--layer-alt);
  color: var(--text-2);
  font: var(--fontSizeBase200) / 26px var(--fontFamilyBase);
  white-space: nowrap;
  cursor: pointer;
  transition: background var(--dur-fast) var(--ease);
}

.sbtn:hover {
  background: var(--card-hover);
}

.rec {
  display: grid;
  grid-template-columns: 28px minmax(0, 1fr);
  align-items: center;
  gap: 4px;
  text-align: center;
}

.sbtn.record {
  display: grid;
  place-items: center;
  width: 28px;
  padding: 0;
  color: var(--crit);
}

.sbtn.record svg {
  width: 14px;
  height: 14px;
}

.sbtn.record[aria-pressed='true'] {
  background: var(--crit);
  color: var(--colorNeutralForegroundInverted);
  border-color: transparent;
}

.sbtn.mute[aria-pressed='true'] {
  background: var(--crit);
  color: var(--colorNeutralForegroundInverted);
  border-color: transparent;
}
</style>
