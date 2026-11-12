<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import type { HotkeyBindingView, HotkeyModeView, HotkeyTargetView, HotkeyView } from '@/api/types'
import { useAppStore } from '@/stores/app'
import { captureFromKey, captureFromPointer, formatHotkey, type CaptureResult } from './hotkeyLabels'

// 一个热键的编辑控件：节点设置、连线的"热键…"对话框和热键页共用，改的是同一份数据。
// 录入只在窗口有焦点时进行，由界面自己监听键鼠事件。Esc 取消，Backspace 清除，点别处或窗口失去焦点都回到录入前的状态。
const props = defineProps<{
  target: HotkeyTargetView
  binding: HotkeyBindingView | null
  /** 录音和显示/隐藏主窗口只能用切换 */
  toggleOnly?: boolean
  occupied?: boolean
}>()
const { t } = useI18n()
const store = useAppStore()

const capturing = ref(false)
const mode = computed<HotkeyModeView>(() => props.binding?.mode ?? 'toggle')

async function apply(binding: HotkeyBindingView | null): Promise<void> {
  await store.run(() => backend.setHotkey(props.target, binding))
}

/** 键盘的切换型必须带修饰键：单键直接录成按住型，用户可以再改 */
function modeFor(key: HotkeyView, wanted: HotkeyModeView): HotkeyModeView {
  const bare =
    key.key.type === 'keyboard' &&
    !key.modifiers.ctrl &&
    !key.modifiers.alt &&
    !key.modifiers.shift &&
    !key.modifiers.win
  return bare && wanted === 'toggle' && !props.toggleOnly ? 'hold' : wanted
}

/** 录入期间暂停全部热键，否则按下的组合会被系统热键吞掉，或者直接触发操作 */
function setPaused(paused: boolean): void {
  backend.setHotkeysPaused(paused).catch((error: unknown) => store.showToast(store.errorText(error)))
}

/** 开始录入：接管整个窗口的按键和鼠标按下；窗口失去焦点就取消 */
function startCapture(): void {
  capturing.value = true
  window.addEventListener('keydown', onCaptureKey, true)
  window.addEventListener('pointerdown', onCapturePointer, true)
  window.addEventListener('blur', endCapture)
  setPaused(true)
}

function endCapture(): void {
  capturing.value = false
  window.removeEventListener('keydown', onCaptureKey, true)
  window.removeEventListener('pointerdown', onCapturePointer, true)
  window.removeEventListener('blur', endCapture)
  setPaused(false)
}

/** 结束录入，按结果设定、清除或保持原来的热键 */
function finish(result: CaptureResult): void {
  endCapture()
  if (result.type === 'captured') {
    void apply({ key: result.key, mode: modeFor(result.key, mode.value) })
  } else if (result.type === 'cleared') {
    void apply(null)
  }
}

// 在捕获阶段拦下：Esc 不能关掉对话框，Delete 不能删掉选中的节点
function onCaptureKey(event: KeyboardEvent): void {
  event.preventDefault()
  event.stopPropagation()
  const result = captureFromKey(event)
  if (result !== null) {
    finish(result)
  }
}

function onCapturePointer(event: PointerEvent): void {
  finish(captureFromPointer(event))
}

onBeforeUnmount(() => {
  if (capturing.value) {
    endCapture()
  }
})

function setMode(event: Event): void {
  if (props.binding !== null) {
    void apply({ ...props.binding, mode: (event.target as HTMLSelectElement).value as HotkeyModeView })
  }
}
</script>

<template>
  <div class="hotkey-field">
    <button type="button" class="key" :class="{ capturing }" :disabled="capturing" @click="startCapture">
      {{ capturing ? t('hotkey.pressKey') : binding === null ? t('hotkey.none') : formatHotkey(binding.key) }}
    </button>
    <select
      class="select mode"
      :value="mode"
      :disabled="binding === null || toggleOnly"
      :aria-label="t('hotkey.mode')"
      @change="setMode"
    >
      <button><selectedcontent></selectedcontent></button>
      <option value="toggle">{{ t('hotkey.toggle') }}</option>
      <option value="hold">{{ t('hotkey.hold') }}</option>
    </select>
    <button v-if="binding !== null" type="button" class="clear" :aria-label="t('hotkey.clear')" @click="apply(null)">
      {{ t('hotkey.clear') }}
    </button>
    <span v-if="occupied" class="occupied">{{ t('hotkey.occupied') }}</span>
  </div>
</template>

<style scoped>
.hotkey-field {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}

.key {
  flex: none;
  width: 180px;
  height: 32px;
  padding: 0 8px;
  border-radius: var(--r-ctl);
  border: 1px solid var(--stroke-strong);
  background: var(--layer-alt);
  color: var(--text);
  font: var(--fontSizeBase200) / 30px var(--fontFamilyBase);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  cursor: pointer;
}

.key:hover {
  background: var(--card-hover);
}

.key.capturing {
  border-color: var(--accent);
  color: var(--accent-text);
}

/* 对话框里的 .field .select 是 100% 宽，这里要按内容自适应，否则会撑出水平滚动条 */
.hotkey-field .mode {
  flex: none;
  width: auto;
}

.clear {
  flex: none;
  height: 28px;
  padding: 0 6px;
  border: 0;
  border-radius: var(--r-ctl);
  background: transparent;
  color: var(--accent-text);
  font: var(--fontSizeBase200) / 28px var(--fontFamilyBase);
  cursor: pointer;
}

.clear:hover {
  background: var(--card-hover);
}

.occupied {
  color: var(--crit);
  font-size: var(--fontSizeBase200);
  white-space: nowrap;
}
</style>
