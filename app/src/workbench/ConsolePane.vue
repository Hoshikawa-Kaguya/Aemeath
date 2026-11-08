<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import type { NodeView } from '@/api/types'
import AppIcon from '@/components/AppIcon.vue'
import { openMenu, type MenuItem } from '@/menu/contextMenu'
import { useAppStore } from '@/stores/app'
import StripCard from './StripCard.vue'

// 控制台（GLOSSARY：控制台、固定）：被固定的节点以竖排通道条呈现，顺序保存在配置里，拖动通道条的标题可以排序。
const props = defineProps<{ nodeMenu: (node: NodeView) => MenuItem[] }>()
const { t } = useI18n()
const store = useAppStore()

const pinned = computed(() =>
  (store.state?.console ?? []).flatMap((id) => {
    const node = store.nodeById.get(id)
    return node === undefined ? [] : [node]
  }),
)
const unpinned = computed(() => store.nodes.filter((node) => !(store.state?.console ?? []).includes(node.id)))

const dragged = ref<number | null>(null)
const dropIndex = ref<number | null>(null)

function pinMenu(event: MouseEvent): void {
  const items: MenuItem[] =
    unpinned.value.length === 0
      ? [{ kind: 'action', label: t('con.allPinned'), disabled: true, run: () => {} }]
      : unpinned.value.map((node) => ({
          kind: 'action',
          label: node.name,
          run: () => void store.run(() => backend.pinNode(node.id, true)),
        }))
  openMenu(event, items)
}

function onDragStart(event: DragEvent, node: NodeView): void {
  dragged.value = node.id
  event.dataTransfer?.setData('text/plain', String(node.id))
  if (event.dataTransfer !== null) {
    event.dataTransfer.effectAllowed = 'move'
  }
}

/** 指针在某条通道条的左半边时插到它前面，右半边时插到它后面 */
function onDragOver(event: DragEvent, index: number): void {
  if (dragged.value === null) {
    return
  }
  event.preventDefault()
  const rect = (event.currentTarget as HTMLElement).getBoundingClientRect()
  dropIndex.value = event.clientX < rect.left + rect.width / 2 ? index : index + 1
}

function onDrop(event: DragEvent): void {
  event.preventDefault()
  const node = dragged.value
  const target = dropIndex.value
  dragged.value = null
  dropIndex.value = null
  if (node === null || target === null) {
    return
  }
  const from = pinned.value.findIndex((pin) => pin.id === node)
  // 移除自己之后，后面的位置都前移一位
  const index = from < target ? target - 1 : target
  if (index !== from) {
    void store.run(() => backend.movePin(node, index))
  }
}

function onDragEnd(): void {
  dragged.value = null
  dropIndex.value = null
}
</script>

<template>
  <section class="console-pane" :aria-label="t('con.title')">
    <div class="pane-head">
      <h2>{{ t('con.title') }}</h2>
      <span class="caption ellipsis">{{ t('con.caption', { n: pinned.length }) }}</span>
      <div class="spacer"></div>
      <fluent-button appearance="subtle" @click="pinMenu">
        <AppIcon slot="start" name="plus" />{{ t('con.pin') }}
      </fluent-button>
    </div>
    <div class="strips" @drop="onDrop" @dragover.prevent>
      <p v-if="pinned.length === 0" class="caption empty">{{ t('con.empty') }}</p>
      <div
        v-for="(node, index) in pinned"
        :key="node.id"
        class="slot"
        :class="{
          'drop-before': dropIndex === index,
          'drop-after': dropIndex === index + 1 && index === pinned.length - 1,
          dragging: dragged === node.id,
        }"
        @dragstart="onDragStart($event, node)"
        @dragover="onDragOver($event, index)"
        @dragend="onDragEnd"
      >
        <StripCard :node="node" @menu="openMenu($event, props.nodeMenu(node))" />
      </div>
    </div>
  </section>
</template>

<style scoped>
.console-pane {
  display: flex;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
}

.pane-head {
  display: flex;
  align-items: center;
  gap: 8px;
  height: 44px;
  flex: none;
  padding: 0 12px 0 16px;
  border-bottom: 1px solid var(--stroke);
}

.pane-head h2 {
  margin: 0;
  font: var(--fontWeightSemibold) var(--fontSizeBase300) / 20px var(--fontFamilyBase);
  white-space: nowrap;
}

.spacer {
  flex: 1;
}

.strips {
  flex: 1;
  display: flex;
  gap: 8px;
  min-height: 0;
  padding: 10px 12px 12px;
  overflow-x: auto;
  overflow-y: hidden;
}

.empty {
  margin: 4px;
}

.slot {
  position: relative;
  display: flex;
  transition: opacity var(--dur-fast) var(--ease);
}

.slot.dragging {
  opacity: 0.4;
}

/* 插入位置的指示线 */
.slot.drop-before::before,
.slot.drop-after::after {
  content: '';
  position: absolute;
  top: 0;
  bottom: 0;
  width: 2px;
  border-radius: 1px;
  background: var(--accent);
}

.slot.drop-before::before {
  left: -5px;
}

.slot.drop-after::after {
  right: -5px;
}
</style>
