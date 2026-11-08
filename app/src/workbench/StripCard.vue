<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import { roleOf, type NodeView } from '@/api/types'
import AppIcon from '@/components/AppIcon.vue'
import { formatGain, formatThreshold } from '@/format/format'
import { isMenuKey } from '@/menu/contextMenu'
import NodeMeter from '@/meters/NodeMeter.vue'
import { useAppStore } from '@/stores/app'
import { nodeCode } from '@/stores/nodeLabels'
import { GAIN_MAX_DB, GAIN_MIN_DB } from './context'
import { GATE_MAX_DB, GATE_OFF_POSITION, gateToPosition, positionToGate } from './gate'
import { throttled } from './throttle'

// 控制台上的一条竖排通道条（原型的 .strip，GLOSSARY：通道条）：节点名、门限（只有输入节点）、电平表与增益推子、静音。
// 宽度固定，长名字截断，悬停显示全名。拖动推子时先改本地值，再节流发给后端。
const props = defineProps<{ node: NodeView }>()
const emit = defineEmits<{ menu: [event: MouseEvent | KeyboardEvent] }>()
const { t } = useI18n()
const store = useAppStore()

/** 推子的步进（dB） */
const GAIN_STEP = 0.5

const isInput = computed(() => roleOf(props.node.kind) === 'in')
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

function onGain(): void {
  adjusting = true
  sendGain(gain.value)
}

function onGate(): void {
  adjusting = true
  sendGate(positionToGate(gatePosition.value))
}

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
    :class="{ muted: node.muted }"
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
    <label class="gate" :class="{ na: !isInput }" :title="gateText">
      <span>{{ t('strip.gate') }}</span>
      <input
        v-model.number="gatePosition"
        class="slider"
        type="range"
        :min="GATE_OFF_POSITION"
        :max="GATE_MAX_DB"
        step="1"
        :disabled="!isInput"
        :aria-label="t('strip.gateAria', { name: node.name })"
        :aria-valuetext="gateText"
        @input="onGate"
        @change="done"
      />
    </label>
    <div class="mid">
      <NodeMeter :node="node.id" :muted="node.muted" orientation="vertical" />
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
    </div>
    <div class="strip-foot">
      <span class="readout">{{ formatGain(gain) }}</span>
      <button
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
/* 宽度固定：长名字不撑开通道条 */
.strip {
  flex: none;
  width: 138px;
  display: grid;
  grid-template-rows: auto auto minmax(0, 1fr) auto;
  gap: 6px;
  padding: 8px;
  border-radius: var(--r-card);
  background: var(--card);
  border: 1px solid var(--stroke);
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

.gate {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  gap: 4px;
  align-items: center;
  font-size: 10px;
  color: var(--text-3);
}

.gate.na {
  visibility: hidden;
}

.mid {
  display: flex;
  justify-content: center;
  gap: 10px;
  min-height: 0;
}

.fader {
  writing-mode: vertical-lr;
  direction: rtl;
  width: 28px;
  height: 100%;
  accent-color: var(--accent);
  cursor: pointer;
}

.strip-foot {
  display: grid;
  gap: 4px;
}

.readout {
  text-align: center;
  font-size: var(--fontSizeBase200);
  color: var(--text-2);
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

.sbtn.mute[aria-pressed='true'] {
  background: var(--crit);
  color: var(--colorNeutralForegroundInverted);
  border-color: transparent;
}
</style>
