<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import type { HotkeyBindingView, HotkeyModeView, HotkeyTargetView, HotkeyView } from '@/api/types'
import { useAppStore } from '@/stores/app'
import { formatHotkey } from './hotkeyLabels'

// 一个热键的编辑控件（M3 计划 H7、H12）：节点设置、连线的"热键…"对话框和热键页共用，改的是同一份数据。
// 录入由后端的低级钩子捕获，WebView 收不到 Win 组合键和鼠标侧键。Esc 取消，Backspace 清除。
const props = defineProps<{
  target: HotkeyTargetView
  binding: HotkeyBindingView | null
  /** 录音和显示/隐藏主窗口只能用切换（M3 计划 H4） */
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

/** 键盘的切换型必须带修饰键（M3 计划 H5）：单键直接录成按住型，用户可以再改 */
function modeFor(key: HotkeyView, wanted: HotkeyModeView): HotkeyModeView {
  const bare =
    key.key.type === 'keyboard' &&
    !key.modifiers.ctrl &&
    !key.modifiers.alt &&
    !key.modifiers.shift &&
    !key.modifiers.win
  return bare && wanted === 'toggle' && !props.toggleOnly ? 'hold' : wanted
}

async function capture(): Promise<void> {
  capturing.value = true
  try {
    const result = await backend.captureHotkey()
    if (result.type === 'captured') {
      await apply({ key: result.key, mode: modeFor(result.key, mode.value) })
    } else if (result.type === 'cleared') {
      await apply(null)
    }
  } catch (error) {
    store.showToast(store.errorText(error))
  } finally {
    capturing.value = false
  }
}

function setMode(event: Event): void {
  if (props.binding !== null) {
    void apply({ ...props.binding, mode: (event.target as HTMLSelectElement).value as HotkeyModeView })
  }
}
</script>

<template>
  <div class="hotkey-field">
    <button type="button" class="key" :class="{ capturing }" :disabled="capturing" @click="capture">
      {{ capturing ? t('hotkey.pressKey') : binding === null ? t('hotkey.none') : formatHotkey(binding.key) }}
    </button>
    <select
      class="select mode"
      :value="mode"
      :disabled="binding === null || toggleOnly"
      :aria-label="t('hotkey.mode')"
      @change="setMode"
    >
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

.mode {
  flex: none;
  width: 110px;
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
