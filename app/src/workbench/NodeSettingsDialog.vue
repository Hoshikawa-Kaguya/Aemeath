<script setup lang="ts">
import { computed, ref, useTemplateRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import { DEFAULT_DEVICE_SETTINGS, roleOf, type DeviceSettings, type NodeView } from '@/api/types'
import { formatThreshold } from '@/format/format'
import { useAppStore } from '@/stores/app'
import { slotName } from '@/stores/nodeLabels'
import DeviceSettingsFields from './DeviceSettingsFields.vue'
import { GATE_MAX_DB, GATE_OFF_POSITION, gateToPosition, positionToGate } from './gate'

// 节点设置（M2 计划 2.2）：从节点的"⋯"按钮或右键菜单打开，尺寸固定，点"确定"才生效。
// 内容随节点类型而定：名称；虚拟节点改选槽位；输入节点的门限；外部设备的独占、格式、缓冲与重采样算法（ADR-0014）；
// 是否固定到控制台。改了设备设置时，该设备的流淡出、重新打开、再淡入。
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

const node = computed(() => (nodeId.value === null ? undefined : store.nodeById.get(nodeId.value)))
const deviceDirection = computed(() => (node.value?.kind === 'externalInput' ? 'input' : 'output'))
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

function open(target: NodeView): void {
  nodeId.value = target.id
  name.value = target.name
  slotNumber.value = target.slot?.number ?? null
  gatePosition.value = gateToPosition(target.gateDb)
  pinned.value = store.state?.console.includes(target.id) === true
  io.value = { ...(target.io ?? DEFAULT_DEVICE_SETTINGS) }
  candidate.value = target.candidates[0]?.endpointId ?? ''
  dialog.value?.show()
}

async function confirm(): Promise<void> {
  const current = node.value
  if (current === undefined) {
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

        <label v-if="slotChoices.length > 0" class="field">
          <span>{{ t('nodeSettings.slot') }}</span>
          <select v-model.number="slotNumber" class="select">
            <option v-for="slot in slotChoices" :key="slot.number" :value="slot.number">{{ slotName(slot) }}</option>
          </select>
          <span class="caption">{{ t('nodeSettings.slotHint') }}</span>
        </label>

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

        <label class="check">
          <input v-model="pinned" type="checkbox" />
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
  max-width: 520px;
  font: var(--fontWeightSemibold) var(--fontSizeBase500) / var(--lineHeightBase500) var(--fontFamilyBase);
}

/* 尺寸固定，长名字不撑开对话框 */
.form {
  display: flex;
  flex-direction: column;
  gap: 16px;
  width: 520px;
}

.check {
  display: flex;
  align-items: center;
  gap: 8px;
}

.check input {
  width: 16px;
  height: 16px;
  margin: 0;
  accent-color: var(--accent);
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

.field-title {
  font-weight: var(--fontWeightSemibold);
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
