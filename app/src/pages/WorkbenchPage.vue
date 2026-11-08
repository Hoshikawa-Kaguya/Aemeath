<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, provide, ref, useTemplateRef } from 'vue'
import {
  ConnectionMode,
  VueFlow,
  useVueFlow,
  type Edge,
  type Node,
  type NodeDragEvent,
  type OnConnectStartParams,
} from '@vue-flow/core'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import type { NodeView, Rejection, RouteView } from '@/api/types'
import AppIcon from '@/components/AppIcon.vue'
import { useAppStore } from '@/stores/app'
import { nodeCode, nodeSubtitle } from '@/stores/nodeLabels'
import AddNodeDialog from '@/workbench/AddNodeDialog.vue'
import IoNode from '@/workbench/IoNode.vue'
import PendingRoute from '@/workbench/PendingRoute.vue'
import RouteEdge from '@/workbench/RouteEdge.vue'
import { LABEL_STEP, clampLabel } from '@/workbench/curve'
import {
  GAIN_MAX_DB,
  GAIN_MIN_DB,
  workbenchKey,
  type NodeData,
  type RouteData,
  type WorkbenchActions,
} from '@/workbench/context'

// 工作台（原型的连线图）：节点与连线、拖线校验并说明原因、选中后按 Del 删除、撤销（ADR-0013）。
// 连线规则只在后端定义一次（aemeath_protocol::topology::check_route）：拖线开始时取回全部候选节点的判定，
// 拖动中按指针下方的节点即时提示。放在整个节点上即可连线，不必对准端口（与原型一致）。

/** 点阵背景的间距（工作台坐标） */
const GRID = 20
const FIT_PADDING = 0.2
const FIT_DURATION_MS = 200
/** 新节点放在视口中央，减去节点卡片的一半大小 */
const NODE_HALF_WIDTH = 118
const NODE_HALF_HEIGHT = 30
const PERCENT = 100
/** 打开时按 100% 显示（与原型一致），左上角留一点边距；"适应窗口"由用户点击 */
const INITIAL_VIEWPORT = { x: 24, y: 24, zoom: 1 }

const { t } = useI18n()
const store = useAppStore()
const { screenToFlowCoordinate, fitView, viewport } = useVueFlow()

type Selection = { type: 'node'; id: number } | { type: 'route'; from: number; to: number } | null
const selection = ref<Selection>(null)

interface Connecting {
  /** 每次拖线递增：判定结果回来时用它确认还是同一次拖线（ref 返回代理，不能比较对象本身） */
  attempt: number
  node: number
  fromOutput: boolean
  /** 每个候选节点的判定（null 表示可以连）；后端还没答复时为 null */
  verdicts: Map<number, Rejection | null> | null
}
const connecting = ref<Connecting | null>(null)
let connectAttempts = 0
const hovered = ref<number | null>(null)
const pointer = ref({ x: 0, y: 0 })
const pane = useTemplateRef<HTMLElement>('pane')
const addDialog = useTemplateRef<InstanceType<typeof AddNodeDialog>>('addDialog')
/** 正在拖动的节点：拖动中收到新的状态快照时，不把它拉回保存的位置 */
const dragging = ref<number | null>(null)

const selectedRoute = computed(() => {
  const current = selection.value
  return current?.type === 'route'
    ? store.routes.find((route) => route.from === current.from && route.to === current.to)
    : undefined
})

const hoveredVerdict = computed(() => {
  const current = connecting.value
  if (current === null || current.verdicts === null || hovered.value === null) {
    return undefined
  }
  return current.verdicts.get(hovered.value)
})

const dragTip = computed(() => {
  const current = connecting.value
  if (current === null) {
    return null
  }
  const verdict = hoveredVerdict.value
  const target = hovered.value === null ? undefined : store.nodeById.get(hovered.value)
  if (verdict === undefined || target === undefined) {
    return { text: t('drag.hint'), bad: false }
  }
  const dst = target.name
  switch (verdict) {
    case null:
      return { text: t('drag.ok', { dst }), bad: false }
    case 'selfRoute':
      return { text: t('drag.selfRoute'), bad: true }
    case 'duplicate':
      return { text: t('drag.duplicate'), bad: true }
    case 'loop':
      return { text: t('drag.loop'), bad: true }
    // 往下游拖时目标不能接收；往上游拖时目标不能发送
    case 'cannotReceive':
    case 'cannotSend':
      return { text: t(current.fromOutput ? 'drag.noInput' : 'drag.noOutput', { dst }), bad: true }
  }
  return { text: t('drag.hint'), bad: false }
})

const flowNodes = computed<Node<NodeData>[]>(() =>
  store.nodes.map((node) => {
    const drop =
      connecting.value !== null && hovered.value === node.id && hoveredVerdict.value !== undefined
        ? hoveredVerdict.value === null
          ? 'ok'
          : 'bad'
        : null
    return {
      id: String(node.id),
      type: 'io',
      position: dragging.value === node.id ? currentPosition(node) : { x: node.x, y: node.y },
      data: {
        node,
        code: nodeCode(node, store.nodes),
        subtitle: nodeSubtitle(node),
        selected: selection.value?.type === 'node' && selection.value.id === node.id,
        drop,
      },
    }
  }),
)

const { findNode } = useVueFlow()
function currentPosition(node: NodeView) {
  return findNode(String(node.id))?.position ?? { x: node.x, y: node.y }
}

const flowEdges = computed<Edge<RouteData>[]>(() =>
  store.routes.map((route) => ({
    id: `${route.from}-${route.to}`,
    source: String(route.from),
    target: String(route.to),
    type: 'route',
    data: {
      route,
      sourceName: store.nodeById.get(route.from)?.name ?? '',
      targetName: store.nodeById.get(route.to)?.name ?? '',
      selected:
        selection.value?.type === 'route' && selection.value.from === route.from && selection.value.to === route.to,
    },
  })),
)

const gridStyle = computed(() => {
  const size = GRID * viewport.value.zoom
  return {
    backgroundSize: `${size}px ${size}px`,
    backgroundPosition: `${viewport.value.x}px ${viewport.value.y}px`,
  }
})

const actions: WorkbenchActions = {
  selectRoute(route) {
    selection.value = { type: 'route', from: route.from, to: route.to }
  },
  selectNode(node) {
    selection.value = { type: 'node', id: node.id }
  },
  toggleRoute(route) {
    actions.selectRoute(route)
    void store.run(() => backend.setRouteEnabled(route.from, route.to, !route.enabled))
  },
  nudgeGain(route, steps) {
    actions.selectRoute(route)
    const gain = Math.min(GAIN_MAX_DB, Math.max(GAIN_MIN_DB, Math.round(route.gainDb) + steps))
    if (gain !== route.gainDb) {
      void store.run(() => backend.setRouteGain(route.from, route.to, gain))
    }
  },
  moveLabel(route, position) {
    void store.run(() => backend.setRouteLabel(route.from, route.to, clampLabel(position)))
  },
  rename(node, name) {
    void store.run(() => backend.renameNode(node.id, name))
  },
  toFlow(point) {
    return screenToFlowCoordinate(point)
  },
}
provide(workbenchKey, actions)

function trackPointer(event: PointerEvent): void {
  const rect = pane.value?.getBoundingClientRect()
  pointer.value = { x: event.clientX - (rect?.left ?? 0), y: event.clientY - (rect?.top ?? 0) }
  const element = document.elementFromPoint(event.clientX, event.clientY)
  const id = element?.closest<HTMLElement>('.vue-flow__node')?.dataset.id
  hovered.value = id === undefined ? null : Number(id)
}

function onConnectStart(params: { event?: MouseEvent | TouchEvent } & OnConnectStartParams): void {
  if (params.nodeId === undefined) {
    return
  }
  const node = Number(params.nodeId)
  const fromOutput = params.handleType === 'source'
  const attempt = ++connectAttempts
  connecting.value = { attempt, node, fromOutput, verdicts: null }
  window.addEventListener('pointermove', trackPointer)
  backend
    .routeVerdicts(node, fromOutput)
    .then((verdicts) => {
      if (connecting.value?.attempt === attempt) {
        connecting.value = { ...connecting.value, verdicts: new Map(verdicts.map((v) => [v.node, v.rejection])) }
      }
    })
    .catch((error: unknown) => store.showToast(store.errorText(error)))
}

function onConnectEnd(): void {
  const current = connecting.value
  const target = hovered.value
  const accepted = current !== null && target !== null && hoveredVerdict.value === null
  window.removeEventListener('pointermove', trackPointer)
  connecting.value = null
  hovered.value = null
  if (!accepted || current === null || target === null) {
    return
  }
  const [from, to] = current.fromOutput ? [current.node, target] : [target, current.node]
  void store
    .run(() => backend.connect(from, to))
    .then((ok) => {
      if (ok) {
        selection.value = { type: 'route', from, to }
      }
    })
}

function onNodeDragStart(event: NodeDragEvent): void {
  dragging.value = Number(event.node.id)
}

function onNodeDragStop(event: NodeDragEvent): void {
  dragging.value = null
  const { x, y } = event.node.position
  void store.run(() => backend.moveNode(Number(event.node.id), Math.round(x), Math.round(y)))
}

function addNode(): void {
  const rect = pane.value?.getBoundingClientRect()
  const center = rect
    ? screenToFlowCoordinate({ x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 })
    : { x: 0, y: 0 }
  void addDialog.value?.open({
    x: Math.round(center.x - NODE_HALF_WIDTH),
    y: Math.round(center.y - NODE_HALF_HEIGHT),
  })
}

function fit(): void {
  void fitView({ padding: FIT_PADDING, maxZoom: 1, duration: FIT_DURATION_MS })
}

function isTyping(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest('input, textarea, select, fluent-dialog') !== null
}

function onKeyDown(event: KeyboardEvent): void {
  if (isTyping(event.target)) {
    return
  }
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'z') {
    event.preventDefault()
    if (store.state?.canUndo) {
      void store.undo()
    }
    return
  }
  if (event.key === 'Escape') {
    selection.value = null
    return
  }
  const route = selectedRoute.value
  if (route !== undefined && event.altKey && (event.key === 'ArrowLeft' || event.key === 'ArrowRight')) {
    event.preventDefault()
    actions.moveLabel(route, route.labelPosition + (event.key === 'ArrowRight' ? LABEL_STEP : -LABEL_STEP))
    return
  }
  if (event.key !== 'Delete' && event.key !== 'Backspace') {
    return
  }
  const current = selection.value
  if (current?.type === 'route') {
    event.preventDefault()
    selection.value = null
    void store.disconnect(current.from, current.to)
  } else if (current?.type === 'node') {
    const node = store.nodeById.get(current.id)
    if (node !== undefined) {
      event.preventDefault()
      selection.value = null
      void store.removeNode(node)
    }
  }
}

function onEdgeClick(event: { edge: Edge<RouteData> }): void {
  const route: RouteView | undefined = event.edge.data?.route
  if (route !== undefined) {
    actions.selectRoute(route)
  }
}

onMounted(() => window.addEventListener('keydown', onKeyDown))
onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKeyDown)
  window.removeEventListener('pointermove', trackPointer)
})
</script>

<template>
  <section class="workbench">
    <div class="pane-head">
      <h2>{{ t('nav.workbench') }}</h2>
      <span class="caption ellipsis">{{ t('wb.hint') }}</span>
      <div class="spacer"></div>
      <fluent-button appearance="subtle" :disabled="!store.state?.canUndo" @click="store.undo()">
        <AppIcon slot="start" name="undo" />{{ t('wb.undo') }}
      </fluent-button>
      <fluent-button appearance="subtle" @click="addNode">
        <AppIcon slot="start" name="plus" />{{ t('wb.add') }}
      </fluent-button>
      <fluent-button appearance="subtle" @click="fit">
        <AppIcon slot="start" name="fit" />{{ t('wb.fit') }}
      </fluent-button>
      <span class="caption zoom">{{ t('wb.zoom', { percent: Math.round(viewport.zoom * PERCENT) }) }}</span>
    </div>
    <div ref="pane" class="graph" :style="gridStyle">
      <VueFlow
        :nodes="flowNodes"
        :edges="flowEdges"
        :min-zoom="0.25"
        :max-zoom="2"
        :pan-on-scroll="true"
        :zoom-on-scroll="false"
        zoom-activation-key-code="Control"
        :delete-key-code="null"
        :connection-mode="ConnectionMode.Strict"
        :default-viewport="INITIAL_VIEWPORT"
        :connect-on-click="false"
        :elevate-edges-on-select="false"
        @connect-start="onConnectStart"
        @connect-end="onConnectEnd"
        @node-drag-start="onNodeDragStart"
        @node-drag-stop="onNodeDragStop"
        @edge-click="onEdgeClick"
        @pane-click="selection = null"
      >
        <template #node-io="props">
          <IoNode v-bind="props" />
        </template>
        <template #edge-route="props">
          <RouteEdge v-bind="props" />
        </template>
        <template #connection-line="line">
          <PendingRoute
            :source-x="line.sourceX"
            :source-y="line.sourceY"
            :target-x="line.targetX"
            :target-y="line.targetY"
            :from-output="connecting?.fromOutput ?? true"
            :bad="dragTip?.bad ?? false"
          />
        </template>
      </VueFlow>
      <div
        v-if="dragTip"
        class="drag-tip"
        :class="{ bad: dragTip.bad }"
        :style="{ left: `${pointer.x}px`, top: `${pointer.y}px` }"
      >
        {{ dragTip.text }}
      </div>
    </div>
    <AddNodeDialog ref="addDialog" />
  </section>
</template>

<style scoped>
.workbench {
  display: flex;
  flex-direction: column;
  height: 100%;
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

.zoom {
  flex: none;
  min-width: 40px;
  text-align: right;
}

.graph {
  position: relative;
  flex: 1;
  min-height: 0;
  background-image: radial-gradient(var(--grid-dot) 1.2px, transparent 1.3px);
}

.drag-tip {
  position: absolute;
  z-index: 5;
  pointer-events: none;
  transform: translate(14px, 14px);
  max-width: 360px;
  padding: 3px 8px;
  border-radius: var(--r-ctl);
  font-size: var(--fontSizeBase100);
  line-height: 16px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  background: var(--card-solid);
  border: 1px solid var(--stroke-strong);
  box-shadow: var(--shadow);
}

.drag-tip.bad {
  border-color: var(--crit);
}
</style>
