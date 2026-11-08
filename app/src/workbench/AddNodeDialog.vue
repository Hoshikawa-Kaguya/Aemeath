<script setup lang="ts">
import { computed, ref, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import type { DeviceChoice, NewNodeKind } from '@/api/types'
import { useAppStore } from '@/stores/app'

// "添加节点"对话框：选类型；外部设备再选设备；名称默认取设备名或类型名，可以修改。
// 虚拟节点的槽位由后端自动分配编号最小的空闲槽位（ADR-0009）。
type KindChoice = 'externalInput' | 'virtualInput' | 'virtualOutput' | 'externalOutput'

interface FluentDialog extends HTMLElement {
  show(): void
  hide(): void
}

const KINDS: KindChoice[] = ['externalInput', 'virtualInput', 'virtualOutput', 'externalOutput']
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
let position = { x: 0, y: 0 }

const isExternal = computed(() => kind.value === 'externalInput' || kind.value === 'externalOutput')
const device = computed(() => devices.value?.find((choice) => choice.endpointId === endpointId.value))
const canConfirm = computed(() => name.value.trim() !== '' && (!isExternal.value || device.value !== undefined))

async function loadDevices(): Promise<void> {
  devices.value = null
  endpointId.value = ''
  if (!isExternal.value) {
    return
  }
  const direction = kind.value === 'externalInput' ? 'input' : 'output'
  try {
    const list = await backend.listDevices(direction)
    devices.value = list
    endpointId.value = list[0]?.endpointId ?? ''
  } catch (error) {
    devices.value = []
    store.showToast(store.errorText(error))
  }
}

function suggestName(): void {
  if (nameEdited.value) {
    return
  }
  name.value = (isExternal.value ? device.value?.name : t(`kind.${kind.value}`)) ?? ''
}

watch(kind, async () => {
  await loadDevices()
  suggestName()
})
watch(endpointId, suggestName)

/** 打开对话框；新节点放在 `at`（工作台坐标） */
async function open(at: { x: number; y: number }): Promise<void> {
  position = at
  nameEdited.value = false
  dialog.value?.show()
  await loadDevices()
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
      if (device.value === undefined) {
        return
      }
      newKind = { type: kind.value, endpointId: device.value.endpointId, deviceName: device.value.name }
      break
    default:
      newKind = { type: kind.value }
  }
  if (await store.run(() => backend.addNode(newKind, name.value.trim(), position.x, position.y))) {
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
          <label v-for="choice in KINDS" :key="choice" class="kind">
            <input v-model="kind" type="radio" name="kind" :value="choice" />
            <span class="choice">
              <b>{{ t(`kind.${choice}`) }}</b>
              <span class="caption">{{ t(`add.kindHint.${choice}`) }}</span>
            </span>
          </label>
        </div>

        <label v-if="isExternal" class="field">
          <span>{{ t('add.device') }}</span>
          <select v-if="devices && devices.length > 0" v-model="endpointId" class="select">
            <option v-for="choice in devices" :key="choice.endpointId" :value="choice.endpointId">
              {{ choice.name }}
            </option>
          </select>
          <span v-else class="caption">{{ devices === null ? t('add.loadingDevices') : t('add.noDevices') }}</span>
        </label>

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
  width: 520px;
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

.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

/* 设备名可能很长：下拉框不超过对话框宽度 */
.field .select {
  width: 100%;
}

.text-input {
  height: 32px;
  padding: 0 10px;
  border-radius: var(--r-ctl);
  border: 1px solid var(--stroke-strong);
  border-bottom-color: var(--colorNeutralStrokeAccessible);
  background: var(--card-solid);
  color: var(--text);
  font: inherit;
}

.text-input:focus {
  outline: none;
  border-bottom: 2px solid var(--accent);
}
</style>
