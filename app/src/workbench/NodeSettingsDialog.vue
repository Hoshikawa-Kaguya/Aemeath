<script setup lang="ts">
import { computed, ref, useTemplateRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import {
  DEFAULT_DEVICE_SETTINGS,
  DEFAULT_SLOT_SETTINGS,
  FLAC_MAX_HZ,
  roleOf,
  type DeviceSettings,
  type NodeView,
  type RecorderSettingsView,
  type Resampler,
  type SlotSettings,
} from '@/api/types'
import { formatThreshold } from '@/format/format'
import ProgramIcon from '@/components/ProgramIcon.vue'
import { useAppStore } from '@/stores/app'
import { slotName } from '@/stores/nodeLabels'
import DeviceSettingsFields from './DeviceSettingsFields.vue'
import { GATE_MAX_DB, GATE_OFF_POSITION, gateToPosition, positionToGate } from './gate'
import HotkeyField from './HotkeyField.vue'
import { pickProgram } from './pickProgram'
import ResamplerField from './ResamplerField.vue'
import SlotSettingsFields from './SlotSettingsFields.vue'

// 节点设置：从节点的"⋯"按钮或右键菜单打开，尺寸固定，点"确定"才生效。
// 内容随节点类型而定：名称；虚拟节点改选槽位；输入节点的门限；外部设备的独占、格式、缓冲与重采样算法；
// 应用源的程序与重采样算法；是否固定到控制台。改了设备设置时，该设备的流淡出、重新打开、再淡入。
interface FluentDialog extends HTMLElement {
  show(): void
  hide(): void
}

const NAME_MAX = 64

const { t } = useI18n()
const store = useAppStore()
const dialog = useTemplateRef<FluentDialog>('dialog')

const nodeId = ref<number | null>(null)
const name = ref('')
const slotNumber = ref<number | null>(null)
const gatePosition = ref(GATE_OFF_POSITION)
const pinned = ref(false)
const io = ref<DeviceSettings>({ ...DEFAULT_DEVICE_SETTINGS })
/** 多个同名候选中选中的端点 ID */
const candidate = ref('')
const appResampler = ref<Resampler | null>(null)
const slotSettings = ref<SlotSettings>({ ...DEFAULT_SLOT_SETTINGS })
const recorder = ref<RecorderSettingsView>({ format: 'flac', depth: 'int24' })

const node = computed(() => (nodeId.value === null ? undefined : store.nodeById.get(nodeId.value)))
const deviceDirection = computed(() => (node.value?.kind === 'externalOutput' ? 'output' : 'input'))
/** 外部插入的送出端在 Windows 里的设备名，用户要在外部程序里选它 */
const sendDevice = computed(() => {
  const current = node.value
  if (current?.kind !== 'externalInsert' || current.slot === null) {
    return null
  }
  const { direction, number } = current.slot
  const row = store.state?.slots.find((slot) => slot.slot.direction === direction && slot.slot.number === number)
  return row?.deviceName ?? slotName(current.slot)
})

/** FLAC 不支持浮点 */
function fixDepth(): void {
  if (recorder.value.format === 'flac' && recorder.value.depth === 'float32') {
    recorder.value.depth = 'int24'
  }
}

/** 复制送出端的设备名，方便粘贴到外部程序里查找 */
function copySend(): void {
  if (sendDevice.value !== null) {
    navigator.clipboard.writeText(sendDevice.value).catch(() => store.showToast(t('nodeSettings.copyFailed')))
  }
}
const isInput = computed(() => node.value !== undefined && roleOf(node.value.kind) === 'in')
/** 同方向、没有被其他节点占用的槽位，加上节点自己现在的槽位 */
const slotChoices = computed(() => {
  const own = node.value?.slot
  if (own === null || own === undefined) {
    return []
  }
  return (store.state?.slots ?? [])
    .filter((row) => row.slot.direction === own.direction && (row.occupant === null || row.occupant === nodeId.value))
    .map((row) => row.slot)
})
const gateText = computed(() => {
  const gate = positionToGate(gatePosition.value)
  return gate === null ? t('nodeSettings.gateOff') : formatThreshold(gate)
})
const canConfirm = computed(() => node.value !== undefined && name.value.trim() !== '')

/** 用节点当前的设置填好表单后打开；改动要点"确定"才生效 */
function open(target: NodeView): void {
  nodeId.value = target.id
  name.value = target.name
  slotNumber.value = target.slot?.number ?? null
  gatePosition.value = gateToPosition(target.gateDb)
  pinned.value = store.state?.console.includes(target.id) === true
  io.value = { ...(target.io ?? DEFAULT_DEVICE_SETTINGS) }
  candidate.value = target.candidates[0]?.endpointId ?? ''
  appResampler.value = target.resampler
  slotSettings.value = { ...(target.slotSettings ?? DEFAULT_SLOT_SETTINGS) }
  if (target.recorder !== null) {
    recorder.value = { ...target.recorder.settings }
  }
  dialog.value?.show()
}

/** 立即改绑（与"改用同名设备"一样，是对程序缺失的处理，不等"确定"） */
async function chooseProgram(): Promise<void> {
  const current = node.value
  const path = await pickProgram()
  if (current !== undefined && path !== null) {
    await store.run(() => backend.replaceProgram(current.id, path))
  }
}

/** 只提交改过的设置；改格式会打断正在使用设备的应用，先确认 */
async function confirm(): Promise<void> {
  const current = node.value
  if (current === undefined) {
    return
  }
  // 改格式会让端点上已有的流作废，正在使用这个设备的应用要重新打开
  const formatChanged =
    current.slotSettings !== null &&
    JSON.stringify(slotSettings.value.format) !== JSON.stringify(current.slotSettings.format)
  if (
    formatChanged &&
    current.status.state === 'ok' &&
    !(await store.confirm(t('slot.changeConfirm'), t('slot.changeAction')))
  ) {
    return
  }
  const trimmed = name.value.trim()
  const steps: (() => Promise<boolean>)[] = []
  if (trimmed !== current.name) {
    steps.push(() => store.run(() => backend.renameNode(current.id, trimmed)))
  }
  if (slotNumber.value !== null && current.slot !== null && slotNumber.value !== current.slot.number) {
    const number = slotNumber.value
    steps.push(() => store.run(() => backend.changeSlot(current.id, number)))
  }
  const gate = positionToGate(gatePosition.value)
  if (isInput.value && gate !== current.gateDb) {
    steps.push(() => store.run(() => backend.setNodeGate(current.id, gate)))
  }
  if (current.io !== null && JSON.stringify(io.value) !== JSON.stringify(current.io)) {
    const settings = io.value
    steps.push(() => store.run(() => backend.setDeviceSettings(current.id, settings)))
  }
  if (current.slotSettings !== null && JSON.stringify(slotSettings.value) !== JSON.stringify(current.slotSettings)) {
    const settings = slotSettings.value
    steps.push(() => store.run(() => backend.setSlotSettings(current.id, settings)))
  }
  if (current.program !== null && JSON.stringify(appResampler.value) !== JSON.stringify(current.resampler)) {
    const resampler = appResampler.value
    steps.push(() => store.run(() => backend.setAppResampler(current.id, resampler)))
  }
  if (
    current.recorder !== null &&
    (recorder.value.format !== current.recorder.settings.format ||
      recorder.value.depth !== current.recorder.settings.depth)
  ) {
    const settings = { ...recorder.value }
    steps.push(() => store.run(() => backend.setRecorderSettings(current.id, settings)))
  }
  const wasPinned = store.state?.console.includes(current.id) === true
  if (pinned.value !== wasPinned) {
    const pin = pinned.value
    steps.push(() => store.run(() => backend.pinNode(current.id, pin)))
  }
  for (const step of steps) {
    if (!(await step())) {
      return
    }
  }
  dialog.value?.hide()
}

/** 立即改用选中的同名设备（不等"确定"：这是对离线状态的处理，不是设置） */
async function useCandidate(): Promise<void> {
  const current = node.value
  const choice = current?.candidates.find((device) => device.endpointId === candidate.value)
  if (current !== undefined && choice !== undefined) {
    await store.run(() => backend.replaceDevice(current.id, choice.endpointId, choice.name))
  }
}

defineExpose({ open })
</script>

<template>
  <fluent-dialog ref="dialog" type="modal">
    <fluent-dialog-body>
      <h2 slot="title" class="title ellipsis" :title="node?.name">
        {{ t('nodeSettings.title', { name: node?.name ?? '' }) }}
      </h2>
      <div v-if="node" class="form">
        <div
          v-if="node.candidates.length > 0"
          class="candidates"
          role="group"
          :aria-label="t('nodeSettings.candidates')"
        >
          <span class="field-title">{{ t('nodeSettings.candidates') }}</span>
          <span class="caption">{{ t('nodeSettings.candidatesHint', { device: node.device?.name ?? '' }) }}</span>
          <div class="row">
            <select v-model="candidate" class="select">
              <option v-for="choice in node.candidates" :key="choice.endpointId" :value="choice.endpointId">
                {{ choice.name }} · {{ choice.endpointId }}
              </option>
            </select>
            <fluent-button :disabled="candidate === ''" @click="useCandidate">
              {{ t('nodeSettings.useCandidate') }}
            </fluent-button>
          </div>
        </div>

        <label class="field">
          <span>{{ t('add.name') }}</span>
          <input v-model="name" class="text-input" :maxlength="NAME_MAX" />
        </label>

        <div v-if="node.program" class="field">
          <span>{{ t('add.program') }}</span>
          <span class="program-path">
            <ProgramIcon :path="node.program.path" />
            <span class="caption ellipsis" :title="node.program.path">{{ node.program.path }}</span>
          </span>
          <span v-if="node.status.state === 'programMissing'" class="caption missing">
            {{ t('node.programMissing') }}
          </span>
          <div>
            <fluent-button
              :appearance="node.status.state === 'programMissing' ? 'primary' : undefined"
              @click="chooseProgram"
            >
              {{ t('node.chooseProgram') }}
            </fluent-button>
          </div>
          <span class="caption">{{ t('add.appSourceNote') }}</span>
        </div>

        <label v-if="slotChoices.length > 0" class="field">
          <span>{{ t('nodeSettings.slot') }}</span>
          <select v-model.number="slotNumber" class="select">
            <option v-for="slot in slotChoices" :key="slot.number" :value="slot.number">{{ slotName(slot) }}</option>
          </select>
          <span class="caption">{{ t('nodeSettings.slotHint') }}</span>
        </label>

        <div v-if="sendDevice" class="field">
          <span class="caption">{{ t('nodeSettings.insertHint', { device: sendDevice }) }}</span>
          <div>
            <fluent-button @click="copySend">{{ t('nodeSettings.copy') }}</fluent-button>
          </div>
        </div>

        <div v-if="isInput" class="field">
          <span class="row">
            <label for="gate-threshold">{{ t('strip.gate') }}</label>
            <span class="value">{{ gateText }}</span>
          </span>
          <input
            id="gate-threshold"
            v-model.number="gatePosition"
            class="slider"
            type="range"
            :min="GATE_OFF_POSITION"
            :max="GATE_MAX_DB"
            step="1"
            :aria-valuetext="gateText"
          />
          <span class="caption">{{ t('nodeSettings.gateHint') }}</span>
        </div>

        <DeviceSettingsFields
          v-if="node.device !== null"
          v-model="io"
          :endpoint-id="node.device.endpointId"
          :direction="deviceDirection"
        />

        <ResamplerField v-if="node.program" v-model="appResampler" />

        <template v-if="node.slotSettings">
          <b v-if="node.kind === 'externalInsert'">{{ t('slot.sendTitle') }}</b>
          <SlotSettingsFields v-model="slotSettings" />
        </template>

        <template v-if="node.recorder">
          <label class="field">
            <span>{{ t('rec.defaultFormat') }}</span>
            <select v-model="recorder.format" class="select" @change="fixDepth">
              <option value="flac">{{ t('rec.format.flac') }}</option>
              <option value="wav">{{ t('rec.format.wav') }}</option>
            </select>
          </label>
          <label class="field">
            <span>{{ t('rec.defaultDepth') }}</span>
            <select v-model="recorder.depth" class="select">
              <option value="int16">{{ t('rec.depth.int16') }}</option>
              <option value="int24">{{ t('rec.depth.int24') }}</option>
              <option value="float32" :disabled="recorder.format === 'flac'">{{ t('rec.depth.float32') }}</option>
            </select>
            <span class="caption">{{ t('rec.settingsHint') }}</span>
            <span
              v-if="recorder.format === 'flac' && (store.state?.engine.sampleRate ?? 0) > FLAC_MAX_HZ"
              class="caption"
            >
              {{ t('rec.flacRateHint') }}
            </span>
          </label>
        </template>

        <div class="field">
          <span>{{ t('hotkey.muteLabel') }}</span>
          <HotkeyField :target="{ type: 'nodeMute', node: node.id }" :binding="node.muteHotkey" />
        </div>

        <div v-if="node.recorder" class="field">
          <span>{{ t('hotkey.recordLabel') }}</span>
          <HotkeyField :target="{ type: 'nodeRecord', node: node.id }" :binding="node.recordHotkey" toggle-only />
        </div>

        <label class="check">
          <span class="switch">
            <input v-model="pinned" type="checkbox" role="switch" />
            <span class="track" aria-hidden="true"><span class="thumb"></span></span>
          </span>
          <span>{{ t('menu.pin') }}</span>
        </label>
      </div>
      <fluent-button slot="action" @click="dialog?.hide()">{{ t('add.cancel') }}</fluent-button>
      <fluent-button slot="action" appearance="primary" :disabled="!canConfirm" @click="confirm">
        {{ t('nodeSettings.confirm') }}
      </fluent-button>
    </fluent-dialog-body>
  </fluent-dialog>
</template>

<style scoped>
.title {
  margin: 0;
  font: var(--fontWeightSemibold) var(--fontSizeBase500) / var(--lineHeightBase500) var(--fontFamilyBase);
}

.form {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.check {
  display: flex;
  align-items: center;
  gap: 8px;
}

.candidates {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 10px 12px;
  border-radius: var(--r-card);
  border: 1px solid color-mix(in srgb, var(--warn) 45%, transparent);
  background: color-mix(in srgb, var(--warn) 10%, transparent);
}

.candidates .select {
  flex: 1;
  min-width: 0;
}

.program-path {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
}

.field-title {
  font-weight: var(--fontWeightSemibold);
}

.missing {
  color: var(--warn);
}

.row {
  display: flex;
  justify-content: space-between;
  gap: 12px;
}

.value {
  color: var(--text-2);
  font-variant-numeric: tabular-nums;
}
</style>
