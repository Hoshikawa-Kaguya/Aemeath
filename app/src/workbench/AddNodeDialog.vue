<script setup lang="ts">
import { computed, ref, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import {
  DEFAULT_DEVICE_SETTINGS,
  DEFAULT_SLOT_SETTINGS,
  type DeviceChoice,
  type DeviceSettings,
  type NewNodeKind,
  type NodeView,
  type Resampler,
  type SlotSettings,
} from '@/api/types'
import { useAppStore } from '@/stores/app'
import { slotName } from '@/stores/nodeLabels'
import DeviceSettingsFields from './DeviceSettingsFields.vue'
import { pickProgram } from './pickProgram'
import ResamplerField from './ResamplerField.vue'
import SlotSettingsFields from './SlotSettingsFields.vue'

// "添加节点"对话框：选类型；外部设备再选设备，并在添加时就设定独占、格式、缓冲和重采样算法（ADR-0014，
// 维护者要求避免添加后再调整）；名称默认取设备名或类型名，可以修改。
// 虚拟节点的槽位由后端自动分配编号最小的空闲槽位（ADR-0009）。总线不对应任何设备，只有名称。
// 应用源：从有音频会话的程序里选，或者"浏览…"选还没运行的 exe（M3 计划 A5）。
type KindChoice =
  | 'externalInput'
  | 'virtualInput'
  | 'appSource'
  | 'externalInsert'
  | 'bus'
  | 'recorder'
  | 'virtualOutput'
  | 'externalOutput'

interface FluentDialog extends HTMLElement {
  show(): void
  hide(): void
}

/** 程序列表里的一项 */
interface ProgramChoice {
  path: string
  label: string
}

/** 两列排布，每行两种：两种输入、两种输出、应用源和外部插入、总线和录音节点 */
const KINDS: KindChoice[] = [
  'externalInput',
  'virtualInput',
  'virtualOutput',
  'externalOutput',
  'appSource',
  'externalInsert',
  'bus',
  'recorder',
]
const WIDE: KindChoice[] = []
const NAME_MAX = 64

const emit = defineEmits<{ added: [] }>()
const { t } = useI18n()
const store = useAppStore()

const dialog = useTemplateRef<FluentDialog>('dialog')
const kind = ref<KindChoice>('externalInput')
const devices = ref<DeviceChoice[] | null>(null)
const endpointId = ref('')
const name = ref('')
/** 用户改过名称后，换设备或类型不再覆盖它 */
const nameEdited = ref(false)
const io = ref<DeviceSettings>({ ...DEFAULT_DEVICE_SETTINGS })
let position = { x: 0, y: 0 }

const isInsert = computed(() => kind.value === 'externalInsert')
const isExternal = computed(() => kind.value === 'externalInput' || kind.value === 'externalOutput' || isInsert.value)
const direction = computed(() => (kind.value === 'externalOutput' ? 'output' : 'input'))
/** 外部插入将使用的送出端：编号最小的空闲 Out，与后端的分配规则相同 */
const sendSlot = computed(
  () => store.state?.slots.find((row) => row.slot.direction === 'out' && row.occupant === null)?.slot ?? null,
)

/** 同方向上已被占用的设备不能再添加，说明被谁占用（M3 计划 X6） */
function deviceOwner(endpointId: string): NodeView | undefined {
  const wanted = direction.value === 'input' ? ['externalInput', 'externalInsert'] : ['externalOutput']
  return store.nodes.find((node) => wanted.includes(node.kind) && node.device?.endpointId === endpointId)
}

function deviceLabel(choice: DeviceChoice): string {
  const owner = deviceOwner(choice.endpointId)
  if (owner === undefined) {
    return choice.name
  }
  const key = owner.kind === 'externalInsert' ? 'add.usedAsReturn' : 'add.usedByNode'
  return `${choice.name} (${t(key, { node: owner.name })})`
}
const device = computed(() => devices.value?.find((choice) => choice.endpointId === endpointId.value))
const programs = ref<ProgramChoice[]>([])
const program = ref('')
const programName = ref('')
const appResampler = ref<Resampler | null>(null)
const slotSettings = ref<SlotSettings>({ ...DEFAULT_SLOT_SETTINGS })
/** 这种节点占用槽位：虚拟节点和外部插入的送出端 */
const hasSlot = computed(() => kind.value === 'virtualInput' || kind.value === 'virtualOutput' || isInsert.value)
const isAppSource = computed(() => kind.value === 'appSource')
const canConfirm = computed(
  () =>
    name.value.trim() !== '' &&
    (!isExternal.value || device.value !== undefined) &&
    (!isInsert.value || sendSlot.value !== null) &&
    (!isAppSource.value || program.value !== ''),
)

/** 有音频会话、还没有应用源节点的程序（同一个 exe 只列一次） */
async function loadPrograms(): Promise<void> {
  programs.value = []
  program.value = ''
  programName.value = ''
  if (!isAppSource.value) {
    return
  }
  try {
    const { apps } = await backend.listApps()
    const seen = new Set<string>()
    for (const app of apps) {
      if (!app.system && app.executable !== null && app.sourceNode === null && !seen.has(app.executable)) {
        seen.add(app.executable)
        programs.value.push({ path: app.executable, label: `${app.name} · ${app.executable}` })
      }
    }
  } catch (error) {
    store.showToast(store.errorText(error))
  }
}

async function browse(): Promise<void> {
  const path = await pickProgram()
  if (path !== null) {
    if (!programs.value.some((choice) => choice.path === path)) {
      programs.value.push({ path, label: path })
    }
    program.value = path
  }
}

watch(program, async (path) => {
  programName.value = path === '' ? '' : await backend.describeProgram(path)
  suggestName()
})

async function loadDevices(): Promise<void> {
  devices.value = null
  endpointId.value = ''
  if (!isExternal.value) {
    return
  }
  try {
    const list = await backend.listDevices(direction.value)
    devices.value = list
    endpointId.value = list.find((choice) => deviceOwner(choice.endpointId) === undefined)?.endpointId ?? ''
  } catch (error) {
    devices.value = []
    store.showToast(store.errorText(error))
  }
}

function suggestName(): void {
  if (nameEdited.value) {
    return
  }
  if (isAppSource.value) {
    name.value = programName.value
    return
  }
  if (isInsert.value) {
    name.value = insertName(device.value?.name ?? '')
    return
  }
  name.value = (isExternal.value ? device.value?.name : t(`kind.${kind.value}`)) ?? ''
}

/** 外部插入的默认名取设备名括号里的部分，没有括号时用完整设备名（M3 计划 X2） */
function insertName(deviceName: string): string {
  return /\(([^()]+)\)\s*$/.exec(deviceName)?.[1] ?? deviceName
}

watch(kind, async () => {
  await Promise.all([loadDevices(), loadPrograms()])
  suggestName()
})
watch(endpointId, () => {
  suggestName()
  // 换了设备，上一台设备的格式不一定适用：回到默认值
  io.value = { ...DEFAULT_DEVICE_SETTINGS }
})

/** 打开对话框；新节点放在 `at`（工作台坐标） */
async function open(at: { x: number; y: number }): Promise<void> {
  position = at
  nameEdited.value = false
  io.value = { ...DEFAULT_DEVICE_SETTINGS }
  appResampler.value = null
  slotSettings.value = { ...DEFAULT_SLOT_SETTINGS }
  dialog.value?.show()
  await Promise.all([loadDevices(), loadPrograms()])
  suggestName()
}

function onNameInput(event: Event): void {
  name.value = (event.target as HTMLInputElement).value
  nameEdited.value = true
}

async function confirm(): Promise<void> {
  let newKind: NewNodeKind
  switch (kind.value) {
    case 'externalInput':
    case 'externalOutput':
    case 'externalInsert':
      if (device.value === undefined) {
        return
      }
      newKind = { type: kind.value, endpointId: device.value.endpointId, deviceName: device.value.name }
      break
    case 'appSource':
      newKind = { type: 'appSource', program: program.value, resampler: appResampler.value }
      break
    default:
      newKind = { type: kind.value }
  }
  const settings = isExternal.value ? io.value : null
  const slot = hasSlot.value ? slotSettings.value : null
  if (await store.run(() => backend.addNode(newKind, name.value.trim(), position.x, position.y, settings, slot))) {
    dialog.value?.hide()
    emit('added')
  }
}

defineExpose({ open })
</script>

<template>
  <fluent-dialog ref="dialog" type="modal">
    <fluent-dialog-body>
      <h2 slot="title" class="title">{{ t('add.title') }}</h2>
      <div class="form">
        <div class="kinds" role="radiogroup" :aria-label="t('add.kind')">
          <label v-for="choice in KINDS" :key="choice" class="kind" :class="{ wide: WIDE.includes(choice) }">
            <input v-model="kind" type="radio" name="kind" :value="choice" />
            <span class="choice">
              <b>{{ t(`kind.${choice}`) }}</b>
              <span class="caption">{{ t(`add.kindHint.${choice}`) }}</span>
            </span>
          </label>
        </div>

        <label v-if="isExternal" class="field">
          <span>{{ t(isInsert ? 'add.returnDevice' : 'add.device') }}</span>
          <select v-if="devices && devices.length > 0" v-model="endpointId" class="select">
            <option
              v-for="choice in devices"
              :key="choice.endpointId"
              :value="choice.endpointId"
              :disabled="deviceOwner(choice.endpointId) !== undefined"
            >
              {{ deviceLabel(choice) }}
            </option>
          </select>
          <span v-else class="caption">{{ devices === null ? t('add.loadingDevices') : t('add.noDevices') }}</span>
        </label>

        <div v-if="isAppSource" class="field">
          <label for="add-program">{{ t('add.program') }}</label>
          <div class="program-row">
            <select id="add-program" v-model="program" class="select">
              <option value="" disabled>{{ t('add.programPlaceholder') }}</option>
              <option v-for="choice in programs" :key="choice.path" :value="choice.path">{{ choice.label }}</option>
            </select>
            <fluent-button @click="browse">{{ t('add.browse') }}</fluent-button>
          </div>
          <span class="caption">{{ t('add.appSourceNote') }}</span>
        </div>

        <ResamplerField v-if="isAppSource" v-model="appResampler" />

        <p v-if="isInsert" class="caption insert-send">
          {{ sendSlot === null ? t('error.noFreeSlot') : t('add.insertSend', { slot: slotName(sendSlot) }) }}
        </p>

        <DeviceSettingsFields
          v-if="isExternal && device"
          v-model="io"
          :endpoint-id="device.endpointId"
          :direction="direction"
        />

        <template v-if="hasSlot">
          <b v-if="isInsert">{{ t('slot.sendTitle') }}</b>
          <SlotSettingsFields v-model="slotSettings" />
        </template>

        <label class="field">
          <span>{{ t('add.name') }}</span>
          <input class="text-input" :value="name" :maxlength="NAME_MAX" @input="onNameInput" />
        </label>
      </div>
      <fluent-button slot="action" @click="dialog?.hide()">{{ t('add.cancel') }}</fluent-button>
      <fluent-button slot="action" appearance="primary" :disabled="!canConfirm" @click="confirm">
        {{ t('add.confirm') }}
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

.kinds {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 8px;
}

/* 选项卡片（原型设置页的 .rs-opt） */
.kind {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  gap: 10px;
  padding: 10px 12px;
  border-radius: var(--r-card);
  border: 1px solid var(--stroke-strong);
  background: var(--layer);
  cursor: pointer;
}

.kind.wide {
  grid-column: 1 / -1;
}

.kind:hover {
  background: var(--card-hover);
}

.kind:has(input:checked) {
  border-color: var(--accent);
  background: var(--accent-soft);
}

.kind input {
  margin: 3px 0 0;
  accent-color: var(--accent);
  width: 16px;
  height: 16px;
}

.choice {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.choice b {
  font-weight: var(--fontWeightSemibold);
}

.insert-send {
  margin: 0;
}

.program-row {
  display: flex;
  gap: 8px;
}

.program-row .select {
  flex: 1;
  min-width: 0;
}
</style>
