<script setup lang="ts">
import { computed, inject, nextTick, ref, useTemplateRef } from 'vue'
import { Handle, Position, type NodeProps } from '@vue-flow/core'
import { useI18n } from 'vue-i18n'
import { roleOf } from '@/api/types'
import AppIcon, { type IconName } from '@/components/AppIcon.vue'
import NodeMeter from '@/meters/NodeMeter.vue'
import { workbenchKey, type NodeData } from './context'

// 工作台上的节点卡片（原型的 .node）：类型图标、名称、副标题、代号、电平或离线提示，左右两侧是端口。
// 宽度固定，长名字截断（节点名最长 64 个字符），悬停显示全名；双击名称可以改名。
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
const offlineText = computed(() => {
  const status = node.value.status
  return status.state === 'offline' ? t(`node.offline.${status.reason}`) : null
})

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
  const name = draft.value.trim()
  if (name !== '' && name !== node.value.name) {
    actions?.rename(node.value, name)
  }
}
</script>

<template>
  <div
    class="node"
    :data-role="role"
    :class="{
      sel: data.selected,
      offline: node.status.state === 'offline',
      'drop-ok': data.drop === 'ok',
      'drop-bad': data.drop === 'bad',
    }"
    role="group"
    :aria-label="t('node.aria', { kind: t(`kind.${node.kind}`), name: node.name })"
    @pointerdown="actions?.selectNode(node)"
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
          @keydown.esc.stop.prevent="editing = false"
          @blur="commitRename"
        />
        <b v-else class="ellipsis" :title="t('node.rename', { name: node.name })" @dblclick="startRename">{{
          node.name
        }}</b>
        <span :title="data.subtitle">{{ data.subtitle }}</span>
      </div>
      <span class="code">{{ data.code }}</span>
    </div>
    <div v-if="offlineText" class="node-status" role="status">
      <AppIcon name="warn" /><span class="ellipsis">{{ offlineText }}</span>
    </div>
    <div v-else-if="node.status.state === 'pending'" class="node-status pending" role="status">
      <AppIcon name="sync" /><span class="ellipsis">{{ t('node.pending') }}</span>
    </div>
    <div v-else class="node-body">
      <NodeMeter :node="node.id" :muted="node.muted" />
    </div>
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
