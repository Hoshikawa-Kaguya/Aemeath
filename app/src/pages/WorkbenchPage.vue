<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, provide, ref, useTemplateRef, watch } from 'vue'
import {
  ConnectionMode,
  VueFlow,
  getRectOfNodes,
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
import { SEPARATOR, isMenuKey, openMenu, openMenuAt, type MenuItem } from '@/menu/contextMenu'
import { useAppStore } from '@/stores/app'
import { nodeCode, nodeSubtitle } from '@/stores/nodeLabels'
import AddNodeDialog from '@/workbench/AddNodeDialog.vue'
import IoNode from '@/workbench/IoNode.vue'
import ConsolePane from '@/workbench/ConsolePane.vue'
import NodeSettingsDialog from '@/workbench/NodeSettingsDialog.vue'
import RouteHotkeyDialog from '@/workbench/RouteHotkeyDialog.vue'
import RouteMatrix, { type RoutePair } from '@/workbench/RouteMatrix.vue'
import PendingRoute from '@/workbench/PendingRoute.vue'
import RouteEdge from '@/workbench/RouteEdge.vue'
import { LABEL_MIDDLE, LABEL_STEP, clampLabel } from '@/workbench/curve'
import {
  GAIN_MAX_DB,
  GAIN_MIN_DB,
  workbenchKey,
  type NodeData,
  type RouteData,
  type WorkbenchActions,
} from '@/workbench/context'

// 工作台：节点与连线、拖线校验并说明原因、选中后按 Del 删除、撤销。
// 连线规则只在后端定义一次（aemeath_protocol::topology::check_route）：拖线开始时取回全部候选节点的判定，
// 拖动中按指针下方的节点即时提示。放在整个节点上即可连线，不必对准端口，小小的端口不好瞄准。

/** 点阵背景的间距（工作台坐标） */
const GRID = 20
const MIN_ZOOM = 0.25
const MAX_ZOOM = 2
/** 自适应时，内容离工作台边缘的最小距离（屏幕像素） */
const FIT_MARGIN_PX = 10
/** 端口圆点伸出节点卡片左右两侧的距离（工作台坐标） */
const PORT_OVERHANG = 8
const FIT_DURATION_MS = 200
/** 滚轮每像素的缩放量（2 的指数）：一格滚轮（100 像素）约 4%，Vue Flow 默认是 15% */
const WHEEL_ZOOM_RATE = 0.0006
/** 新节点放在视口中央，减去节点卡片的一半大小 */
const NODE_HALF_WIDTH = 118
const NODE_HALF_HEIGHT = 30
const PERCENT = 100
/** 工作台为空时的视口；有节点时进入页面会自适应（onNodesInitialized） */
const INITIAL_VIEWPORT = { x: 24, y: 24, zoom: 1 }

const { t } = useI18n()
const store = useAppStore()
const { screenToFlowCoordinate, setViewport, getNodes, viewport, d3Zoom } = useVueFlow()

watch(
  d3Zoom,
  (zoom) => {
    zoom?.wheelDelta((event: WheelEvent) => -event.deltaY * WHEEL_ZOOM_RATE)
  },
  { immediate: true },
)

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
const settingsDialog = useTemplateRef<InstanceType<typeof NodeSettingsDialog>>('settingsDialog')
const routeHotkeyDialog = useTemplateRef<InstanceType<typeof RouteHotkeyDialog>>('routeHotkeyDialog')
/** 从菜单选了"改名"的节点 */
const renaming = ref<number | null>(null)
/** 路由矩阵里悬停的格子：工作台上高亮对应的连线和两端节点 */
const matrixHover = ref<RoutePair | null>(null)
const selectedPair = computed<RoutePair | null>(() =>
  selection.value?.type === 'route' ? { from: selection.value.from, to: selection.value.to } : null,
)

function onMatrixSelect(pair: RoutePair | null): void {
  selection.value = pair === null ? null : { type: 'route', ...pair }
}
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
        renaming: renaming.value === node.id,
        highlighted: matrixHover.value?.from === node.id || matrixHover.value?.to === node.id,
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
      highlighted: matrixHover.value?.from === route.from && matrixHover.value.to === route.to,
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
  renameDone() {
    renaming.value = null
  },
  openNodeMenu(event, node) {
    openMenu(event, nodeMenu(node))
  },
  openRouteMenu(event, route) {
    openMenu(event, routeMenu(route))
  },
  openSettings(node) {
    settingsDialog.value?.open(node)
  },
  toFlow(point) {
    return screenToFlowCoordinate(point)
  },
}
provide(workbenchKey, actions)

function nodeMenu(node: NodeView): MenuItem[] {
  const pinned = store.state?.console.includes(node.id) === true
  const isClock = store.state?.engine.clockMaster === node.id
  return [
    { kind: 'action', label: t('menu.nodeSettings'), run: () => settingsDialog.value?.open(node) },
    { kind: 'action', label: t('menu.rename'), run: () => (renaming.value = node.id) },
    {
      kind: 'action',
      label: t(pinned ? 'menu.unpin' : 'menu.pin'),
      run: () => void store.run(() => backend.pinNode(node.id, !pinned)),
    },
    ...(node.kind === 'externalInput' || node.kind === 'externalOutput'
      ? [
          {
            kind: 'action' as const,
            label: t(isClock ? 'menu.clearClock' : 'menu.setClock'),
            run: () => void store.run(() => backend.setClockMaster(isClock ? null : node.id)),
          },
        ]
      : []),
    ...(node.device === null && node.program === null
      ? []
      : [
          {
            kind: 'action' as const,
            label: t('menu.redetect'),
            run: () =>
              void backend.redetect(node.id).catch((error: unknown) => store.showToast(store.errorText(error))),
          },
        ]),
    SEPARATOR,
    { kind: 'action', label: t('menu.deleteNode'), danger: true, run: () => void store.removeNode(node) },
  ]
}

function routeMenu(route: RouteView): MenuItem[] {
  return [
    { kind: 'action', label: t('menu.hotkey'), run: () => routeHotkeyDialog.value?.open(route) },
    {
      kind: 'action',
      label: t(route.enabled ? 'menu.routeOff' : 'menu.routeOn'),
      run: () => actions.toggleRoute(route),
    },
    {
      kind: 'action',
      label: t('menu.resetGain'),
      disabled: route.gainDb === 0,
      run: () => void store.run(() => backend.setRouteGain(route.from, route.to, 0)),
    },
    {
      kind: 'action',
      label: t(route.labelVisible ? 'menu.hideLabel' : 'menu.showLabel'),
      run: () => void store.run(() => backend.setRouteLabelVisible(route.from, route.to, !route.labelVisible)),
    },
    {
      kind: 'action',
      label: t('menu.resetLabel'),
      disabled: !route.labelVisible || route.labelPosition === LABEL_MIDDLE,
      run: () => actions.moveLabel(route, LABEL_MIDDLE),
    },
    SEPARATOR,
    {
      kind: 'action',
      label: t('menu.deleteRoute'),
      danger: true,
      run: () => {
        selection.value = null
        void store.disconnect(route.from, route.to)
      },
    },
  ]
}

function paneMenu(at: { x: number; y: number }): MenuItem[] {
  return [
    { kind: 'action', label: t('menu.addHere'), run: () => openAddDialog(at) },
    { kind: 'action', label: t('menu.fit'), disabled: store.nodes.length === 0, run: fit },
  ]
}

function onPaneContextMenu(event: MouseEvent): void {
  selection.value = null
  const at = screenToFlowCoordinate({ x: event.clientX, y: event.clientY })
  openMenu(event, paneMenu({ x: Math.round(at.x), y: Math.round(at.y) }))
}

/** 节点卡片自己处理右键（IoNode）；连线标签也是（RouteEdge）。这里只处理连线路径本身 */
function onEdgeContextMenu(event: { event: MouseEvent | TouchEvent; edge: Edge<RouteData> }): void {
  const route = event.edge.data?.route
  if (event.event instanceof MouseEvent && route !== undefined) {
    actions.selectRoute(route)
    openMenu(event.event, routeMenu(route))
  }
}

/** 键盘打开菜单（菜单键或 Shift+F10）：针对当前选中的节点或连线，放在它旁边 */
function openSelectionMenu(): boolean {
  const current = selection.value
  if (current?.type === 'node') {
    const node = store.nodeById.get(current.id)
    const element = document.querySelector(`.vue-flow__node[data-id="${current.id}"]`)
    if (node !== undefined && element !== null) {
      const rect = element.getBoundingClientRect()
      openMenuAt({ x: rect.left, y: rect.bottom }, nodeMenu(node))
      return true
    }
  } else if (current?.type === 'route') {
    const route = selectedRoute.value
    const rect = pane.value?.getBoundingClientRect()
    if (route !== undefined && rect !== undefined) {
      openMenuAt({ x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 }, routeMenu(route))
      return true
    }
  }
  return false
}

/** 拖线期间跟踪指针：记下位置（画虚线用）和指针下方的节点（判定能否连线用） */
function trackPointer(event: PointerEvent): void {
  const rect = pane.value?.getBoundingClientRect()
  pointer.value = { x: event.clientX - (rect?.left ?? 0), y: event.clientY - (rect?.top ?? 0) }
  const element = document.elementFromPoint(event.clientX, event.clientY)
  const id = element?.closest<HTMLElement>('.vue-flow__node')?.dataset.id
  hovered.value = id === undefined ? null : Number(id)
}

/**
 * 开始拖线：向后端要所有节点的连线判定，拖动中据此即时提示。
 * 判定是异步取回的，用 `attempt` 丢掉上一次拖线迟到的结果
 */
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

/** 松开拖线：落在允许连线的节点上就建立连线并选中它，否则什么都不做 */
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

/** 拖完节点后把新位置存到后端 */
async function onNodeDragStop(event: NodeDragEvent): Promise<void> {
  const id = Number(event.node.id)
  const { x, y } = event.node.position
  await store.run(() => backend.moveNode(id, Math.round(x), Math.round(y)))
  // 等后端返回新坐标再松手，否则节点会先退回旧坐标闪一下；等待期间又开始拖动时不能松手
  if (dragging.value === id && findNode(event.node.id)?.dragging !== true) {
    dragging.value = null
  }
}

function addNode(): void {
  const rect = pane.value?.getBoundingClientRect()
  const center = rect
    ? screenToFlowCoordinate({ x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 })
    : { x: 0, y: 0 }
  openAddDialog({ x: Math.round(center.x), y: Math.round(center.y) })
}

function applyPreset(): void {
  void store.run(() => backend.applyPreset(t('preset.inputName'), t('preset.outputName')))
}

/** 新节点以 `center`（工作台坐标）为中心 */
function openAddDialog(center: { x: number; y: number }): void {
  void addDialog.value?.open({ x: center.x - NODE_HALF_WIDTH, y: center.y - NODE_HALF_HEIGHT })
}

/** 全部节点缩放到正好放下并居中：较紧的一边离工作台边缘 FIT_MARGIN_PX */
function fit(duration = FIT_DURATION_MS): void {
  const element = pane.value
  if (element === null || getNodes.value.length === 0) {
    return
  }
  const bounds = getRectOfNodes(getNodes.value)
  const scale = Math.min(
    (element.clientWidth - 2 * FIT_MARGIN_PX) / (bounds.width + 2 * PORT_OVERHANG),
    (element.clientHeight - 2 * FIT_MARGIN_PX) / bounds.height,
  )
  const zoom = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, scale))
  void setViewport(
    {
      x: element.clientWidth / 2 - (bounds.x + bounds.width / 2) * zoom,
      y: element.clientHeight / 2 - (bounds.y + bounds.height / 2) * zoom,
      zoom,
    },
    { duration },
  )
}

/** 进入页面（启动或切换回来）时自适应一次；之后新增节点不再动视口 */
let fittedOnEntry = false
function onNodesInitialized(): void {
  if (!fittedOnEntry && getNodes.value.length > 0) {
    fittedOnEntry = true
    fit(0)
  }
}

/** 焦点在输入框或对话框里时，工作台的快捷键不生效，免得打字时误删节点 */
function isTyping(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest('input, textarea, select, fluent-dialog') !== null
}

/** 工作台的快捷键：Ctrl+Z 撤销、Esc 取消选中、菜单键打开菜单、Del 删除选中的对象 */
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
  if (isMenuKey(event) && openSelectionMenu()) {
    event.preventDefault()
    return
  }
  const route = selectedRoute.value
  if (route?.labelVisible === true && event.altKey && (event.key === 'ArrowLeft' || event.key === 'ArrowRight')) {
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
    <div class="graph-pane">
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
        <fluent-button appearance="subtle" @click="fit()">
          <AppIcon slot="start" name="fit" />{{ t('wb.fit') }}
        </fluent-button>
        <span class="caption zoom">{{ t('wb.zoom', { percent: Math.round(viewport.zoom * PERCENT) }) }}</span>
      </div>
      <ul v-if="store.state?.duplicates.length" class="duplicates" role="status">
        <li v-for="duplicate in store.state.duplicates" :key="`${duplicate.program}-${duplicate.destination}`">
          <AppIcon name="warn" />
          <span class="ellipsis" :title="t('node.duplicate', { ...duplicate })">
            {{ t('node.duplicate', { ...duplicate }) }}
          </span>
        </li>
      </ul>
      <div ref="pane" class="graph" :style="gridStyle">
        <VueFlow
          :nodes="flowNodes"
          :edges="flowEdges"
          :min-zoom="MIN_ZOOM"
          :max-zoom="MAX_ZOOM"
          :delete-key-code="null"
          :zoom-on-double-click="false"
          :connection-mode="ConnectionMode.Strict"
          :default-viewport="INITIAL_VIEWPORT"
          :connect-on-click="false"
          :elevate-edges-on-select="false"
          @nodes-initialized="onNodesInitialized"
          @connect-start="onConnectStart"
          @connect-end="onConnectEnd"
          @node-drag-start="onNodeDragStart"
          @node-drag-stop="onNodeDragStop"
          @edge-click="onEdgeClick"
          @pane-click="selection = null"
          @pane-context-menu="onPaneContextMenu"
          @edge-context-menu="onEdgeContextMenu"
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
        <!-- 工作台为空时的引导（GLOSSARY：工作台预设）：添加了任何节点后消失 -->
        <div v-if="store.state !== null && store.nodes.length === 0" class="empty-guide" role="region">
          <h3>{{ t('preset.title') }}</h3>
          <p class="caption">{{ t('preset.body') }}</p>
          <div class="guide-actions">
            <fluent-button appearance="primary" @click="applyPreset">{{ t('preset.use') }}</fluent-button>
            <fluent-button @click="addNode">{{ t('wb.add') }}</fluent-button>
          </div>
        </div>
      </div>
    </div>
    <RouteMatrix
      class="matrix"
      :selected="selectedPair"
      :route-menu="routeMenu"
      @select="onMatrixSelect"
      @hover="matrixHover = $event"
    />
    <ConsolePane class="console" :node-menu="nodeMenu" />
    <AddNodeDialog ref="addDialog" />
    <NodeSettingsDialog ref="settingsDialog" />
    <RouteHotkeyDialog ref="routeHotkeyDialog" />
  </section>
</template>

<style scoped>
/* 三栏布局：工作台 | 路由矩阵，下方整行是控制台 */
.workbench {
  display: grid;
  grid-template-columns: minmax(0, 1fr) fit-content(50%);
  grid-template-rows: minmax(0, 1fr) 292px;
  height: 100%;
}

.graph-pane {
  display: flex;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
  grid-column: 1;
  grid-row: 1;
  border-right: 1px solid var(--stroke);
  /* 窗口变窄时标题栏的按钮不能溢出到路由矩阵上 */
  overflow: hidden;
}

.matrix {
  grid-column: 2;
  grid-row: 1;
}

.console {
  grid-column: 1 / span 2;
  grid-row: 2;
  border-top: 1px solid var(--stroke);
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

/* 重复路由的文字警告：占自己的一行，不盖住节点；多条时最多显示三行，其余滚动 */
.duplicates {
  flex: none;
  max-height: 88px;
  overflow-y: auto;
  margin: 0;
  padding: 4px 16px;
  list-style: none;
  border-bottom: 1px solid color-mix(in srgb, var(--warn) 45%, transparent);
  background: color-mix(in srgb, var(--warn) 10%, transparent);
  font-size: var(--fontSizeBase200);
}

.duplicates li {
  display: flex;
  align-items: center;
  gap: 6px;
  height: 26px;
}

.duplicates svg {
  flex: none;
  width: 16px;
  height: 16px;
  color: var(--warn);
}

.graph {
  position: relative;
  flex: 1;
  min-height: 0;
  background-image: radial-gradient(var(--grid-dot) 1.2px, transparent 1.3px);
}

/* 空工作台的引导卡片：居中、固定宽度 */
.empty-guide {
  position: absolute;
  left: 50%;
  top: 50%;
  z-index: 4;
  width: 380px;
  transform: translate(-50%, -50%);
  padding: 20px 24px;
  border-radius: var(--r-card);
  background: var(--card-solid);
  border: 1px solid var(--stroke);
  box-shadow: var(--shadow16);
  text-align: center;
}

.empty-guide h3 {
  margin: 0 0 6px;
  font: var(--fontWeightSemibold) var(--fontSizeBase400) / var(--lineHeightBase400) var(--fontFamilyBase);
}

.empty-guide p {
  margin: 0 0 16px;
}

.guide-actions {
  display: flex;
  justify-content: center;
  gap: 8px;
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
