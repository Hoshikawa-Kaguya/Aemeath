<script setup lang="ts">
import { computed, inject, nextTick, ref, useTemplateRef, watch } from 'vue'
import { Handle, Position, type NodeProps } from '@vue-flow/core'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import { roleOf } from '@/api/types'
import AppIcon, { type IconName } from '@/components/AppIcon.vue'
import ProgramIcon from '@/components/ProgramIcon.vue'
import { formatKhz } from '@/format/format'
import { isMenuKey } from '@/menu/contextMenu'
import NodeMeter from '@/meters/NodeMeter.vue'
import { useAppStore } from '@/stores/app'
import { NODE_NAME_MAX, isRecording, recordedTime } from '@/stores/nodeLabels'
import { workbenchKey, type NodeData } from './context'

// 工作台上的节点卡片：类型图标、名称、副标题、代号、状态标签（按需显示）、电平或离线提示，左右两侧是端口。
// 宽度固定，长名字截断（节点名最长 64 个字符），悬停显示全名；未锁定时双击名称或从菜单选"改名"可以改名。
// 右上角的"⋯"按钮与右键打开同一个节点菜单。
// 外部设备在线时显示实际使用的格式和独占与否；离线时醒目标出原因，并给出"重新检测"，
// 独占打不开时另给"改用共享模式"（不自动退回共享）；锁定时修改类快捷按钮置灰并提示原因。
const props = defineProps<NodeProps<NodeData>>()
const { t } = useI18n()
const actions = inject(workbenchKey)

const node = computed(() => props.data.node)
const role = computed(() => roleOf(node.value.kind))
/** 应用源的限制（延迟、抓不到独占播放）在节点上悬停可见 */
const kindHint = computed(() => (node.value.kind === 'appSource' ? t('add.appSourceNote') : undefined))
const icon = computed<IconName>(() => {
  switch (node.value.kind) {
    case 'externalInput':
      return 'mic'
    case 'externalOutput':
      return 'spk'
    case 'bus':
      return 'bus'
    case 'appSource':
      return 'apps'
    case 'externalInsert':
      return 'fx'
    case 'recorder':
      return 'rec'
    default:
      return 'virt'
  }
})
const store = useAppStore()
const isClock = computed(() => store.state?.engine.clockMaster === node.value.id)
const hasBadges = computed(
  () => node.value.locked || node.value.muted || isClock.value || node.value.duplicate !== null,
)
const offlineText = computed(() => {
  const status = node.value.status
  switch (status.state) {
    case 'offline':
      return t(`node.offline.${status.reason}`)
    case 'programMissing':
      return t('node.programMissing')
    default:
      return null
  }
})
const recording = computed(() => isRecording(node.value))
const recordedLabel = computed(() => recordedTime(node.value, store.state?.engine.sampleRate ?? 1) ?? '')
const programMissing = computed(() => node.value.status.state === 'programMissing')
const notRunning = computed(() => node.value.status.state === 'notRunning')
const isExternal = computed(() => node.value.device !== null)
/**
 * 独占用不了时提供"改用共享模式"：被占用、不允许独占，以及无信号——有的设备独占时根本不交付数据
 * （实测 Realtek USB Audio 的 Rear Mic 独占时事件驱动和轮询都读不到包，共享时正常）
 */
const canUseShared = computed(() => {
  const status = node.value.status
  return (
    node.value.io?.exclusive === true &&
    status.state === 'offline' &&
    (status.reason === 'inUseByOtherApp' || status.reason === 'exclusiveNotAllowed' || status.reason === 'noSignal')
  )
})
const formatLine = computed(() => {
  const format = node.value.format
  if (format === null || node.value.io === null) {
    return null
  }
  return t('node.formatLine', {
    rate: formatKhz(format.sampleRate),
    format: t(`sampleFormat.${format.sampleFormat}`),
    mode: t(node.value.io.exclusive ? 'node.exclusive' : 'node.shared'),
  })
})
const lockedActionTip = computed(() => (node.value.locked ? t('node.lockedActionTip') : undefined))

function redetect(): void {
  backend.redetect(node.value.id).catch((error: unknown) => store.showToast(store.errorText(error)))
}

function useShared(): void {
  if (node.value.locked) {
    return
  }
  const io = node.value.io
  if (io !== null) {
    void store.run(() => backend.setDeviceSettings(node.value.id, { ...io, exclusive: false }))
  }
}

function openSettingsIfUnlocked(): void {
  if (!node.value.locked) {
    actions?.openSettings(node.value)
  }
}

const editing = ref(false)
const draft = ref('')
const input = useTemplateRef<HTMLInputElement>('input')

async function startRename(): Promise<void> {
  if (node.value.locked) {
    actions?.renameDone()
    return
  }
  draft.value = node.value.name
  editing.value = true
  await nextTick()
  input.value?.select()
}

function commitRename(): void {
  if (!editing.value) {
    return
  }
  editing.value = false
  actions?.renameDone()
  const name = draft.value.trim()
  if (name !== '' && name !== node.value.name) {
    actions?.rename(node.value, name)
  }
}

function cancelRename(): void {
  editing.value = false
  actions?.renameDone()
}

watch(
  () => props.data.renaming,
  (renaming) => {
    if (renaming && !editing.value) {
      void startRename()
    }
  },
)

watch(
  () => node.value.locked,
  (locked) => {
    if (locked && editing.value) {
      cancelRename()
    }
  },
)

function openMenu(event: MouseEvent | KeyboardEvent): void {
  actions?.selectNode(node.value)
  actions?.openNodeMenu(event, node.value)
}

function onKeyDown(event: KeyboardEvent): void {
  if (isMenuKey(event)) {
    openMenu(event)
  }
}
</script>

<template>
  <div
    class="node"
    :data-role="role"
    :class="{
      sel: data.selected,
      hl: data.highlighted,
      locked: node.locked,
      offline: offlineText !== null,
      idle: notRunning,
      'drop-ok': data.drop === 'ok',
      'drop-bad': data.drop === 'bad',
      leaving: data.leaving,
    }"
    role="group"
    :aria-label="t('node.aria', { kind: t(`kind.${node.kind}`), name: node.name })"
    @pointerdown="actions?.selectNode(node)"
    @contextmenu="openMenu"
    @keydown="onKeyDown"
  >
    <div class="node-head">
      <span v-tip="kindHint" class="kind-ico">
        <ProgramIcon v-if="node.kind === 'appSource'" :path="node.program?.path" /><AppIcon v-else :name="icon" />
      </span>
      <div class="node-title">
        <input
          v-if="editing"
          ref="input"
          v-model="draft"
          class="rename nodrag"
          :maxlength="NODE_NAME_MAX"
          @keydown.enter.prevent="commitRename"
          @keydown.esc.stop.prevent="cancelRename"
          @blur="commitRename"
        />
        <b
          v-else
          v-tip="node.locked ? node.name : t('node.rename', { name: node.name })"
          class="ellipsis"
          @dblclick="startRename"
          >{{ node.name }}</b
        >
        <span v-tip="kindHint" class="ellipsis">{{ data.subtitle.kind }}</span>
        <span v-if="data.subtitle.detail" v-tip="data.subtitle.detail" class="ellipsis">{{
          data.subtitle.detail
        }}</span>
      </div>
      <span class="code">{{ data.code }}</span>
      <button
        type="button"
        class="more nodrag"
        :aria-label="t('menu.aria', { name: node.name })"
        @pointerdown.stop
        @click.stop="openMenu"
      >
        <AppIcon name="more" />
      </button>
    </div>
    <div v-if="hasBadges" class="node-badges">
      <span v-if="node.locked" v-tip="t('node.lockedTip')" class="badge lock">
        <AppIcon name="lock" />{{ t('node.lockedBadge') }}
      </span>
      <span v-if="node.muted" v-tip="t('node.mutedTip')" class="badge mute">
        <AppIcon name="mute" />{{ t('node.mutedBadge') }}
      </span>
      <span v-if="isClock" v-tip="t('node.clockMaster')" class="badge clock">
        <AppIcon name="clock" />{{ t('node.clockMaster') }}
      </span>
      <span
        v-if="node.duplicate"
        v-tip="t('node.duplicate', { program: node.duplicate.program, destination: node.duplicate.destination })"
        class="badge dup"
      >
        <AppIcon name="warn" />{{ t('node.duplicateBadge') }}
      </span>
    </div>
    <template v-if="offlineText">
      <div class="node-status" role="status">
        <AppIcon name="warn" /><span v-tip="offlineText" class="ellipsis">{{ offlineText }}</span>
      </div>
      <div v-if="isExternal || programMissing" class="node-actions nodrag">
        <!-- 锁定时用 aria-disabled 保留鼠标悬浮提示，避免原生 disabled 吞掉 pointerover -->
        <button
          v-if="programMissing"
          v-tip="lockedActionTip"
          type="button"
          class="link"
          :class="{ disabled: node.locked }"
          :aria-disabled="node.locked ? 'true' : undefined"
          @pointerdown.stop
          @click.stop="openSettingsIfUnlocked"
        >
          {{ t('node.chooseProgram') }}
        </button>
        <button
          v-if="node.candidates.length > 0"
          v-tip="lockedActionTip"
          type="button"
          class="link"
          :class="{ disabled: node.locked }"
          :aria-disabled="node.locked ? 'true' : undefined"
          @pointerdown.stop
          @click.stop="openSettingsIfUnlocked"
        >
          {{ t('node.chooseDevice') }}
        </button>
        <button type="button" class="link" @pointerdown.stop @click.stop="redetect">{{ t('node.redetect') }}</button>
        <button
          v-if="canUseShared"
          v-tip="lockedActionTip"
          type="button"
          class="link"
          :class="{ disabled: node.locked }"
          :aria-disabled="node.locked ? 'true' : undefined"
          @pointerdown.stop
          @click.stop="useShared"
        >
          {{ t('node.useShared') }}
        </button>
      </div>
    </template>
    <div v-else-if="node.status.state === 'pending'" class="node-status pending" role="status">
      <AppIcon name="sync" /><span class="ellipsis">{{ t('node.pending') }}</span>
    </div>
    <div
      v-else-if="notRunning"
      v-tip="t('node.notRunningHint', { program: node.program?.fileName ?? '' })"
      class="node-status pending"
      role="status"
    >
      <AppIcon name="unplug" /><span class="ellipsis">{{ t('node.notRunning') }}</span>
    </div>
    <template v-else>
      <div class="node-body">
        <NodeMeter :node="node.id" :muted="node.muted" />
      </div>
      <div v-if="node.recorder" class="rec-row nodrag">
        <button
          type="button"
          class="link"
          :class="{ active: recording }"
          :aria-pressed="recording"
          @pointerdown.stop
          @click.stop="store.toggleRecording(node)"
        >
          <AppIcon :name="recording ? 'stop' : 'rec'" />{{ t(recording ? 'node.stopRecording' : 'node.record') }}
        </button>
        <span class="rec-time">{{ recordedLabel }}</span>
      </div>
      <div v-if="formatLine" v-tip="formatLine" class="node-format ellipsis">{{ formatLine }}</div>
    </template>
    <Handle v-if="role !== 'in'" type="target" :position="Position.Left" class="port in" />
    <Handle v-if="role !== 'out'" type="source" :position="Position.Right" class="port out" />
  </div>
</template>

<style scoped>
.node {
  position: relative;
  width: 236px;
  border-radius: var(--r-card);
  background: var(--card);
  border: 1px solid var(--stroke-strong);
  box-shadow: var(--shadow);
  user-select: none;
  cursor: grab;
  /* 新节点淡入；删除后由工作台保留一会儿，用 .leaving 淡出 */
  animation: node-in var(--dur) var(--ease-enter);
  transition:
    outline-color var(--dur-fast) linear,
    border-color var(--dur-fast) linear,
    background-color var(--dur-fast) linear;
}

@keyframes node-in {
  from {
    opacity: 0;
    transform: scale(0.96);
  }
}

/* 收起比弹出快，期间不可交互 */
.node.leaving {
  pointer-events: none;
  animation: node-out var(--dur-fast) var(--ease-exit) forwards;
}

@keyframes node-out {
  to {
    opacity: 0;
    transform: scale(0.96);
  }
}

.node:active {
  cursor: grabbing;
}

/* 锁定卡片：顶部细边条加底色微调，与选中描边、离线虚线框互不冲突 */
.node.locked {
  background: color-mix(in srgb, var(--colorNeutralBackground2) 82%, transparent);
  border-color: color-mix(in srgb, var(--text-2) 38%, transparent);
}

.node.locked::after {
  content: '';
  position: absolute;
  left: 14px;
  right: 14px;
  top: 0;
  height: 2px;
  border-radius: 0 0 2px 2px;
  background: color-mix(in srgb, var(--text-2) 50%, transparent);
  pointer-events: none;
}

.node.sel {
  outline: 2px solid var(--accent);
  outline-offset: 1px;
}

.node.offline {
  border: 1px dashed var(--warn);
}

/* 应用源的程序没有运行：整体变灰，它不是错误 */
.node.idle {
  opacity: 0.6;
}

/* 路由矩阵里悬停的格子以这个节点为一端 */
.node.hl:not(.sel) {
  outline: 2px solid color-mix(in srgb, var(--accent) 55%, transparent);
  outline-offset: 1px;
}

.node.drop-ok {
  outline: 2px solid var(--accent);
  outline-offset: 2px;
}

.node.drop-bad {
  outline: 2px dashed var(--crit);
  outline-offset: 2px;
}

.node-head {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 10px 6px;
}

.node-title {
  min-width: 0;
  flex: 1;
}

.more {
  display: grid;
  place-items: center;
  flex: none;
  width: 24px;
  height: 24px;
  margin-right: -4px;
  padding: 0;
  border: 0;
  border-radius: var(--r-ctl);
  background: transparent;
  color: var(--text-3);
  cursor: pointer;
}

.more:hover,
.more:focus-visible {
  background: var(--card-hover);
  color: var(--text);
  outline: none;
}

.more svg {
  width: 16px;
  height: 16px;
}

.node-title b {
  display: block;
  font: var(--fontWeightSemibold) var(--fontSizeBase300) / 18px var(--fontFamilyBase);
}

/* 类型和详情各占一行，不随中英文长度折行 */
.node-title span {
  display: block;
  font-size: var(--fontSizeBase100);
  line-height: 14px;
  color: var(--text-3);
}

.rename {
  width: 100%;
  height: 20px;
  padding: 0 4px;
  font: var(--fontWeightSemibold) var(--fontSizeBase300) / 18px var(--fontFamilyBase);
  color: var(--text);
  background: var(--colorNeutralBackground1);
  border: 1px solid var(--accent);
  border-radius: var(--borderRadiusSmall);
  outline: none;
}

/* 状态胶囊标签行：只在节点有锁定、静音、主时钟或重复路由状态时显示 */
.node-badges {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
  padding: 0 10px 6px;
}

.badge {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  height: 18px;
  padding: 0 6px;
  border-radius: var(--borderRadiusCircular);
  border: 1px solid var(--stroke);
  background: var(--layer-alt);
  color: var(--text-2);
  font-size: var(--fontSizeBase100);
  line-height: 16px;
  white-space: nowrap;
}

.badge svg {
  width: 11px;
  height: 11px;
}

.badge.lock {
  border-color: color-mix(in srgb, var(--text-2) 32%, transparent);
  color: var(--text);
}

.badge.mute {
  border-color: color-mix(in srgb, var(--crit) 45%, transparent);
  background: color-mix(in srgb, var(--crit) 12%, transparent);
  color: var(--crit);
}

.badge.clock {
  border-color: color-mix(in srgb, var(--accent) 45%, transparent);
  background: color-mix(in srgb, var(--accent) 12%, transparent);
  color: var(--accent-text);
}

.badge.dup {
  border-color: color-mix(in srgb, var(--warn) 45%, transparent);
  background: color-mix(in srgb, var(--warn) 12%, transparent);
  color: var(--warn);
}

.node-body {
  display: flex;
  align-items: center;
  padding: 2px 10px 9px;
}

.rec-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin: -6px 6px 6px;
}

.rec-row .link {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  color: var(--crit);
}

.rec-row svg {
  width: 12px;
  height: 12px;
}

.rec-time {
  padding-right: 4px;
  font-size: var(--fontSizeBase100);
  color: var(--text-2);
  font-variant-numeric: tabular-nums;
}

.node-format {
  margin: -5px 10px 8px;
  font-size: var(--fontSizeBase100);
  line-height: 14px;
  color: var(--text-3);
}

.node-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
  margin: -4px 6px 8px;
}

.link {
  height: 22px;
  padding: 0 4px;
  border: 0;
  border-radius: var(--r-ctl);
  background: transparent;
  color: var(--accent-text);
  font: var(--fontSizeBase100) / 22px var(--fontFamilyBase);
  cursor: pointer;
}

.link:hover {
  background: var(--card-hover);
}

.link.disabled {
  color: var(--text-dis);
  cursor: not-allowed;
}

.link.disabled:hover {
  background: transparent;
}

.node-status {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 0 10px 9px;
  font-size: var(--fontSizeBase100);
  line-height: 14px;
}

.node-status svg {
  color: var(--warn);
  width: 14px;
  height: 14px;
}

.node-status.pending {
  color: var(--text-3);
}

.node-status.pending svg {
  color: var(--text-3);
}

/* 端口：覆盖 Vue Flow 默认的手柄样式，距顶部 26 px，与标题行对齐 */
.port {
  top: 26px;
  width: 12px;
  height: 12px;
  border-radius: 50%;
  background: var(--card-solid);
  border: 2px solid var(--text-3);
  cursor: crosshair;
}

.port::before {
  content: '';
  position: absolute;
  inset: -8px;
  border-radius: 50%;
}

.port:hover {
  border-color: var(--accent);
  background: var(--accent);
}
</style>
