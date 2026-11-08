<script setup lang="ts">
import { computed, inject, nextTick, ref, useTemplateRef, watch } from 'vue'
import { Handle, Position, type NodeProps } from '@vue-flow/core'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import { roleOf } from '@/api/types'
import AppIcon, { type IconName } from '@/components/AppIcon.vue'
import { formatKhz } from '@/format/format'
import { isMenuKey } from '@/menu/contextMenu'
import NodeMeter from '@/meters/NodeMeter.vue'
import { useAppStore } from '@/stores/app'
import { workbenchKey, type NodeData } from './context'

// 工作台上的节点卡片（原型的 .node）：类型图标、名称、副标题、代号、电平或离线提示，左右两侧是端口。
// 宽度固定，长名字截断（节点名最长 64 个字符），悬停显示全名；双击名称或从菜单选"改名"可以改名。
// 右上角的"⋯"按钮与右键打开同一个节点菜单。
// 外部设备在线时显示实际使用的格式和独占与否；离线时醒目标出原因，并给出"重新检测"，
// 独占打不开时另给"改用共享模式"（ADR-0014：不自动退回共享）。
const props = defineProps<NodeProps<NodeData>>()
const { t } = useI18n()
const actions = inject(workbenchKey)

const node = computed(() => props.data.node)
const role = computed(() => roleOf(node.value.kind))
const icon = computed<IconName>(() => {
  switch (node.value.kind) {
    case 'externalInput':
      return 'mic'
    case 'externalOutput':
      return 'spk'
    case 'bus':
      return 'bus'
    default:
      return 'virt'
  }
})
const store = useAppStore()
const offlineText = computed(() => {
  const status = node.value.status
  return status.state === 'offline' ? t(`node.offline.${status.reason}`) : null
})
const isExternal = computed(() => node.value.device !== null)
/**
 * 独占用不了时提供"改用共享模式"：被占用、不允许独占，以及无信号——有的设备独占时根本不交付数据
 * （2026-11-08 E2E：Realtek USB Audio 的 Rear Mic 独占时事件驱动和轮询都读不到包，共享时正常）
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

function redetect(): void {
  backend.redetect(node.value.id).catch((error: unknown) => store.showToast(store.errorText(error)))
}

function useShared(): void {
  const io = node.value.io
  if (io !== null) {
    void store.run(() => backend.setDeviceSettings(node.value.id, { ...io, exclusive: false }))
  }
}

const editing = ref(false)
const draft = ref('')
const input = useTemplateRef<HTMLInputElement>('input')

async function startRename(): Promise<void> {
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
      offline: node.status.state === 'offline',
      'drop-ok': data.drop === 'ok',
      'drop-bad': data.drop === 'bad',
    }"
    role="group"
    :aria-label="t('node.aria', { kind: t(`kind.${node.kind}`), name: node.name })"
    @pointerdown="actions?.selectNode(node)"
    @contextmenu="openMenu"
    @keydown="onKeyDown"
  >
    <div class="node-head">
      <span class="kind-ico"><AppIcon :name="icon" /></span>
      <div class="node-title">
        <input
          v-if="editing"
          ref="input"
          v-model="draft"
          class="rename nodrag"
          maxlength="64"
          @keydown.enter.prevent="commitRename"
          @keydown.esc.stop.prevent="cancelRename"
          @blur="commitRename"
        />
        <b v-else class="ellipsis" :title="t('node.rename', { name: node.name })" @dblclick="startRename">{{
          node.name
        }}</b>
        <span :title="data.subtitle">{{ data.subtitle }}</span>
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
    <template v-if="offlineText">
      <div class="node-status" role="status">
        <AppIcon name="warn" /><span class="ellipsis" :title="offlineText">{{ offlineText }}</span>
      </div>
      <div v-if="isExternal" class="node-actions nodrag">
        <button
          v-if="node.candidates.length > 0"
          type="button"
          class="link"
          @pointerdown.stop
          @click.stop="actions?.openSettings(node)"
        >
          {{ t('node.chooseDevice') }}
        </button>
        <button type="button" class="link" @pointerdown.stop @click.stop="redetect">{{ t('node.redetect') }}</button>
        <button v-if="canUseShared" type="button" class="link" @pointerdown.stop @click.stop="useShared">
          {{ t('node.useShared') }}
        </button>
      </div>
    </template>
    <div v-else-if="node.status.state === 'pending'" class="node-status pending" role="status">
      <AppIcon name="sync" /><span class="ellipsis">{{ t('node.pending') }}</span>
    </div>
    <template v-else>
      <div class="node-body">
        <NodeMeter :node="node.id" :muted="node.muted" />
      </div>
      <div v-if="formatLine" class="node-format ellipsis" :title="formatLine">{{ formatLine }}</div>
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
  background: var(--card-solid);
  border: 1px solid var(--stroke-strong);
  box-shadow: var(--shadow);
  user-select: none;
  cursor: grab;
  /* 新节点淡入（删除时由 Vue Flow 直接移除，不做淡出） */
  animation: node-in var(--dur) var(--ease-enter);
  transition:
    outline-color var(--dur-fast) linear,
    border-color var(--dur-fast) linear;
}

@keyframes node-in {
  from {
    opacity: 0;
    transform: scale(0.96);
  }
}

.node:active {
  cursor: grabbing;
}

.node.sel {
  outline: 2px solid var(--accent);
  outline-offset: 1px;
}

.node.offline {
  border: 1px dashed var(--warn);
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
  cursor: default;
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

.node-title span {
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  font-size: var(--fontSizeBase100);
  line-height: 14px;
  color: var(--text-3);
  overflow: hidden;
  overflow-wrap: anywhere;
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

.node-body {
  display: flex;
  align-items: center;
  padding: 2px 10px 9px;
}

.node-format {
  margin: -5px 10px 8px;
  font-size: var(--fontSizeBase100);
  line-height: 14px;
  color: var(--text-3);
}

.node-actions {
  display: flex;
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

/* 端口：覆盖 Vue Flow 默认的手柄样式，位置与原型一致（距顶部 26 px） */
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
