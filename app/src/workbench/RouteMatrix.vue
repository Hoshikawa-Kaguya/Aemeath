<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import { roleOf, type NodeView, type Rejection, type Role, type RouteView } from '@/api/types'
import AppIcon from '@/components/AppIcon.vue'
import { formatGain } from '@/format/format'
import { isMenuKey, openMenu, type MenuItem } from '@/menu/contextMenu'
import { useAppStore } from '@/stores/app'
import { nodeCode } from '@/stores/nodeLabels'
import { GAIN_MAX_DB, GAIN_MIN_DB } from './context'
import { createHoverWheel } from './hoverWheel'

// 路由矩阵（GLOSSARY：路由矩阵；M2 计划 2.3）：行是能发出声音的节点（输入、处理），列是能接收声音的节点（处理、输出），
// 每个格子就是一条连线。行高、列宽、行头和列头的宽度都固定，长名字截断，悬停显示全名。
// 不能新建连线的格子（连到自己、会形成回路）禁用，原因来自后端（与拖线时的规则相同）。
export interface RoutePair {
  from: number
  to: number
}

const props = defineProps<{
  /** 与工作台同步的选中连线 */
  selected: RoutePair | null
  routeMenu: (route: RouteView) => MenuItem[]
}>()
const emit = defineEmits<{
  select: [pair: RoutePair | null]
  /** 悬停或聚焦的格子（工作台据此高亮对应的连线和两端节点） */
  hover: [pair: RoutePair | null]
}>()

const { t } = useI18n()
const store = useAppStore()
const pinOnly = ref(false)
const pane = ref<HTMLElement | null>(null)
/** 打开"仅固定节点"前的面板宽度：开关只筛选内容，不改变面板宽度 */
const unfilteredWidth = ref<number | null>(null)
// 默认的 pre 时机在 DOM 更新之前，量到的还是筛选前的宽度
watch(pinOnly, (only) => {
  unfilteredWidth.value = only ? (pane.value?.offsetWidth ?? null) : null
})
const verdicts = ref(new Map<string, Rejection>())
const hovered = ref<RoutePair | null>(null)
/** 键盘焦点所在的格子（行、列下标），整个矩阵只有它可以用 Tab 进入 */
const focusCell = ref({ row: 0, column: 0 })

const key = (from: number, to: number) => `${from}-${to}`
const GROUP_ORDER: Record<Role, number> = { in: 0, proc: 1, out: 2 }

function visible(node: NodeView): boolean {
  return !pinOnly.value || store.state?.console.includes(node.id) === true
}

function sorted(nodes: NodeView[]): NodeView[] {
  return [...nodes].sort((a, b) => GROUP_ORDER[roleOf(a.kind)] - GROUP_ORDER[roleOf(b.kind)] || a.id - b.id)
}

const rows = computed(() => sorted(store.nodes.filter((node) => roleOf(node.kind) !== 'out' && visible(node))))
const columns = computed(() => sorted(store.nodes.filter((node) => roleOf(node.kind) !== 'in' && visible(node))))
const routeMap = computed(() => new Map(store.routes.map((route) => [key(route.from, route.to), route])))

interface Group {
  role: Role
  count: number
}

function groupsOf(nodes: NodeView[]): Group[] {
  const groups: Group[] = []
  for (const node of nodes) {
    const role = roleOf(node.kind)
    const last = groups.at(-1)
    if (last?.role === role) {
      last.count += 1
    } else {
      groups.push({ role, count: 1 })
    }
  }
  return groups
}

const columnGroups = computed(() => groupsOf(columns.value))
/** 每个分组的第一行：在它前面画纵向的分组标题（跨这一组的全部行） */
const rowGroupStart = computed(() => {
  const starts = new Map<number, Group>()
  let index = 0
  for (const group of groupsOf(rows.value)) {
    starts.set(index, group)
    index += group.count
  }
  return starts
})

// 节点或连线变化后重新取不能连线的格子
watch(
  () => [store.nodes, store.routes],
  () => {
    backend
      .matrixVerdicts()
      .then((list) => (verdicts.value = new Map(list.map((v) => [key(v.from, v.to), v.rejection]))))
      .catch(() => {})
  },
  { immediate: true },
)

function rejectionText(rejection: Rejection): string {
  return rejection === 'loop' ? t('drag.loop') : t('drag.selfRoute')
}

function cellText(route: RouteView | undefined): string {
  if (route === undefined) {
    return '+'
  }
  if (!route.enabled) {
    return t('mx.off')
  }
  return formatGain(route.gainDb).replace(' dB', '')
}

function stateText(route: RouteView | undefined): string {
  if (route === undefined) {
    return t('mx.stNone')
  }
  return route.enabled ? t('mx.stOn', { gain: formatGain(route.gainDb) }) : t('mx.stOff')
}

function describe(from: NodeView, to: NodeView): string {
  const route = routeMap.value.get(key(from.id, to.id))
  const rejection = verdicts.value.get(key(from.id, to.id))
  const state = rejection !== undefined && route === undefined ? rejectionText(rejection) : stateText(route)
  const text = t('mx.cell', { src: from.name, dst: to.name, state })
  const duplicate = route?.duplicate
  return duplicate == null
    ? text
    : `${text} · ${t('node.duplicate', { program: duplicate.program, destination: duplicate.destination })}`
}

const info = computed(() => {
  const pair = hovered.value
  const from = pair === null ? undefined : store.nodeById.get(pair.from)
  const to = pair === null ? undefined : store.nodeById.get(pair.to)
  return from === undefined || to === undefined ? null : describe(from, to)
})

function isSelected(from: number, to: number): boolean {
  return props.selected?.from === from && props.selected.to === to
}

async function click(from: NodeView, to: NodeView): Promise<void> {
  const route = routeMap.value.get(key(from.id, to.id))
  if (route === undefined) {
    if (await store.run(() => backend.connect(from.id, to.id))) {
      emit('select', { from: from.id, to: to.id })
    }
    return
  }
  emit('select', { from: from.id, to: to.id })
  await store.run(() => backend.setRouteEnabled(route.from, route.to, !route.enabled))
}

/** 与工作台相同（M2 计划 2.2）：格子已选中、或者按住 Ctrl 时才调增益，滚动矩阵时不会误改 */
/** 同一时间只悬停一个格子，所以整个矩阵共用一个 */
const hoverWheel = createHoverWheel()

/** 与连线标签相同：悬停在格子上滚动即调增益；滚动矩阵时扫过格子不算（hoverWheel.ts）。按住 Ctrl 时总是调增益 */
function onWheel(event: WheelEvent, from: NodeView, to: NodeView): void {
  const route = routeMap.value.get(key(from.id, to.id))
  if (route === undefined || (!hoverWheel.accepts() && !event.ctrlKey)) {
    return
  }
  event.preventDefault()
  emit('select', { from: from.id, to: to.id })
  nudge(route, event.deltaY < 0 ? 1 : -1)
}

function nudge(route: RouteView, steps: number): void {
  const gain = Math.min(GAIN_MAX_DB, Math.max(GAIN_MIN_DB, Math.round(route.gainDb) + steps))
  if (gain !== route.gainDb) {
    void store.run(() => backend.setRouteGain(route.from, route.to, gain))
  }
}

function newRouteMenu(from: NodeView, to: NodeView): MenuItem[] {
  return [{ kind: 'action', label: t('mx.newRoute'), run: () => void click(from, to) }]
}

function onContextMenu(event: MouseEvent | KeyboardEvent, from: NodeView, to: NodeView): void {
  const route = routeMap.value.get(key(from.id, to.id))
  if (route !== undefined) {
    emit('select', { from: from.id, to: to.id })
    openMenu(event, props.routeMenu(route))
  } else if (!verdicts.value.has(key(from.id, to.id))) {
    openMenu(event, newRouteMenu(from, to))
  } else {
    event.preventDefault()
    event.stopPropagation()
  }
}

function focusAt(row: number, column: number): void {
  const rowCount = rows.value.length
  const columnCount = columns.value.length
  if (rowCount === 0 || columnCount === 0) {
    return
  }
  focusCell.value = {
    row: Math.max(0, Math.min(rowCount - 1, row)),
    column: Math.max(0, Math.min(columnCount - 1, column)),
  }
  void nextTick(() => {
    document
      .querySelector<HTMLButtonElement>(
        `.mx .cell[data-row="${focusCell.value.row}"][data-column="${focusCell.value.column}"]`,
      )
      ?.focus()
  })
}

function onKeyDown(event: KeyboardEvent, row: number, column: number, from: NodeView, to: NodeView): void {
  const route = routeMap.value.get(key(from.id, to.id))
  if (isMenuKey(event)) {
    onContextMenu(event, from, to)
    return
  }
  switch (event.key) {
    case 'ArrowUp':
      focusAt(row - 1, column)
      break
    case 'ArrowDown':
      focusAt(row + 1, column)
      break
    case 'ArrowLeft':
      focusAt(row, column - 1)
      break
    case 'ArrowRight':
      focusAt(row, column + 1)
      break
    case 'Delete':
    case 'Backspace':
      if (route === undefined) {
        return
      }
      emit('select', null)
      void store.disconnect(route.from, route.to)
      break
    case '+':
    case '=':
      if (route !== undefined) {
        nudge(route, 1)
      }
      break
    case '-':
      if (route !== undefined) {
        nudge(route, -1)
      }
      break
    default:
      return
  }
  event.preventDefault()
  event.stopPropagation()
}

function setHover(pair: RoutePair | null): void {
  hovered.value = pair
  emit('hover', pair)
}

function isHot(node: NodeView, axis: 'from' | 'to'): boolean {
  return hovered.value?.[axis] === node.id
}
</script>

<template>
  <section ref="pane" class="matrix-pane" :aria-label="t('mx.title')">
    <div v-if="unfilteredWidth !== null" class="width-holder" :style="{ width: `${unfilteredWidth}px` }"></div>
    <div class="pane-head">
      <h2>{{ t('mx.title') }}</h2>
      <div class="spacer"></div>
      <label class="switch">
        <input v-model="pinOnly" type="checkbox" role="switch" />
        <span class="track" aria-hidden="true"><span class="thumb"></span></span>
        <span class="ellipsis">{{ t('mx.pinOnly') }}</span>
      </label>
    </div>
    <div class="matrix-wrap" @mouseleave="setHover(null)">
      <p v-if="rows.length === 0 || columns.length === 0" class="caption empty">{{ t('mx.empty') }}</p>
      <table v-else class="mx">
        <thead>
          <tr class="grp">
            <th class="corner" rowspan="2" colspan="2">
              <span class="to">{{ t('mx.to') }}</span
              ><span class="from">{{ t('mx.from') }}</span>
            </th>
            <th v-for="group in columnGroups" :key="group.role" :colspan="group.count" :data-grp="group.role">
              {{ t(`grp.${group.role}`) }}
            </th>
          </tr>
          <tr>
            <th
              v-for="column in columns"
              :key="column.id"
              class="ch"
              :class="{ hl: isHot(column, 'to') }"
              :title="column.name"
              scope="col"
            >
              <span class="col-code">{{ nodeCode(column, store.nodes) }}</span>
            </th>
          </tr>
        </thead>
        <tbody>
          <template v-for="(row, rowIndex) in rows" :key="row.id">
            <tr v-if="rowIndex > 0 && rowGroupStart.has(rowIndex)" class="gap" aria-hidden="true">
              <td :colspan="columns.length + 2"></td>
            </tr>
            <tr>
              <th
                v-if="rowGroupStart.has(rowIndex)"
                class="vgrp"
                :data-grp="rowGroupStart.get(rowIndex)?.role"
                :rowspan="rowGroupStart.get(rowIndex)?.count"
                scope="rowgroup"
                :title="t(`grp.${rowGroupStart.get(rowIndex)?.role}`)"
              >
                <span>{{ t(`grp.${rowGroupStart.get(rowIndex)?.role}`) }}</span>
              </th>
              <th class="rh" :class="{ hl: isHot(row, 'from') }" :title="row.name" scope="row">
                <b>{{ nodeCode(row, store.nodes) }}</b>
              </th>
              <td v-for="(column, columnIndex) in columns" :key="column.id">
                <span v-if="row.id === column.id" class="cell self" aria-hidden="true"></span>
                <button
                  v-else
                  type="button"
                  class="cell"
                  :class="{
                    on: routeMap.get(key(row.id, column.id))?.enabled === true,
                    off: routeMap.get(key(row.id, column.id))?.enabled === false,
                    sel: isSelected(row.id, column.id),
                    duplicate: routeMap.get(key(row.id, column.id))?.duplicate != null,
                  }"
                  :data-row="rowIndex"
                  :data-column="columnIndex"
                  :tabindex="focusCell.row === rowIndex && focusCell.column === columnIndex ? 0 : -1"
                  :disabled="!routeMap.has(key(row.id, column.id)) && verdicts.has(key(row.id, column.id))"
                  :aria-label="describe(row, column)"
                  :title="describe(row, column)"
                  @click="click(row, column)"
                  @wheel="onWheel($event, row, column)"
                  @contextmenu="onContextMenu($event, row, column)"
                  @keydown="onKeyDown($event, rowIndex, columnIndex, row, column)"
                  @mouseenter="(hoverWheel.enter(), setHover({ from: row.id, to: column.id }))"
                  @focus="
                    ((focusCell = { row: rowIndex, column: columnIndex }), setHover({ from: row.id, to: column.id }))
                  "
                  @blur="setHover(null)"
                >
                  {{ cellText(routeMap.get(key(row.id, column.id))) }}
                </button>
              </td>
            </tr>
          </template>
        </tbody>
      </table>
    </div>
    <div class="mx-info" :class="{ active: info !== null }" role="status">
      <AppIcon :name="info === null ? 'info' : 'graph'" />
      <span class="ellipsis" :title="info ?? undefined">{{ info ?? t('mx.idle') }}</span>
    </div>
    <div class="legend">
      <span><i class="lg-on"></i>{{ t('mx.lgOn') }}</span>
      <span><i class="lg-off"></i>{{ t('mx.lgOff') }}</span>
      <span><i></i>{{ t('mx.lgNone') }}</span>
      <span><i class="lg-dup"></i>{{ t('mx.lgDup') }}</span>
      <span>{{ t('mx.lgKeys') }}</span>
    </div>
  </section>
</template>

<style scoped>
/* 宽度跟随矩阵本身（WorkbenchPage 的 fit-content 列），放不下时在 .matrix-wrap 里滚动 */
.matrix-pane {
  display: flex;
  flex-direction: column;
  /* 放得下图例里最长的一项（中文操作提示约 293 px，加左右内边距），不让它折行 */
  min-width: 340px;
  min-height: 0;
}

/* 只占宽度不占高度；放在面板这层，窗口变窄、列宽封顶时不会让 .matrix-wrap 多出滚动条 */
.width-holder {
  flex: none;
  height: 0;
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

.matrix-wrap {
  flex: 1;
  min-height: 0;
  overflow: auto;
  /* 出现竖向滚动条时不挤占矩阵的宽度，否则又会多出横向滚动条 */
  scrollbar-gutter: stable;
  padding: 8px 8px 12px;
}

/* 提示文字不决定面板宽度，否则矩阵一空面板就被撑长 */
.empty {
  contain: inline-size;
  margin: 12px 8px;
}

/* 行高、列宽固定：格子、行头、列头都有固定尺寸，长名字不撑开行列（维护者 2026-11-08）。
   不用 table-layout: fixed：那样列宽取自带 colspan 的分组标题行，列头会被挤窄 */
table.mx {
  border-collapse: separate;
  border-spacing: 3px;
  --mx-grp-font: var(--fontWeightSemibold) 11px / 14px var(--fontFamilyBase);
}

.mx th {
  font-weight: var(--fontWeightRegular);
  font-size: 11px;
  color: var(--text-3);
}

.mx [data-grp='in'] {
  --hue: var(--in-hue);
}

.mx [data-grp='proc'] {
  --hue: var(--proc-hue);
}

.mx [data-grp='out'] {
  --hue: var(--out-hue);
}

/* 左上角跨分组标题列（20 px）和行头列（48 px） */
.mx .corner {
  position: relative;
  width: 74px;
  height: 44px;
}

.mx .corner::after {
  content: '';
  position: absolute;
  inset: 4px;
  background: linear-gradient(
    to top right,
    transparent calc(50% - 0.5px),
    var(--stroke-strong) 50%,
    transparent calc(50% + 0.5px)
  );
}

.mx .corner .to,
.mx .corner .from {
  position: absolute;
  font: var(--mx-grp-font);
  letter-spacing: 0.5px;
  color: var(--text-2);
}

.mx .corner .to {
  right: 4px;
  top: 2px;
}

.mx .corner .from {
  left: 4px;
  bottom: 2px;
}

.mx thead tr.grp th:not(.corner) {
  height: 20px;
  padding: 0 2px;
  vertical-align: bottom;
  font: var(--mx-grp-font);
  letter-spacing: 0.5px;
  color: var(--text-2);
  text-align: center;
  border-bottom: 2px solid var(--hue);
  white-space: nowrap;
  overflow: hidden;
}

/* 行头、列头只显示代号（A1、IN2、BUS1、OUT3、D1），全名在悬停提示和底部状态行里（维护者 2026-11-08） */
.mx th.ch {
  width: 36px;
  min-width: 36px;
  max-width: 36px;
  height: 22px;
  padding: 0 0 2px;
  vertical-align: bottom;
  text-align: center;
  overflow: hidden;
}

.mx th.ch span {
  display: block;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.mx th.ch .col-code {
  font: var(--fontWeightSemibold) 11px / 14px var(--fontFamilyBase);
  color: var(--text-2);
}

.mx tbody th.vgrp {
  position: relative;
  width: 20px;
  max-width: 20px;
  padding: 0;
  border-right: 2px solid var(--hue);
}

.mx tbody th.vgrp span {
  position: absolute;
  left: 50%;
  top: 50%;
  transform: translate(-50%, -50%);
  writing-mode: vertical-rl;
  font: var(--mx-grp-font);
  letter-spacing: 0.5px;
  color: var(--text-2);
  white-space: nowrap;
  /* 只有一行的分组放不下整个标题：截断，悬停显示全名 */
  max-height: calc(100% - 4px);
  overflow: hidden;
  text-overflow: ellipsis;
}

:lang(en) .mx tbody th.vgrp span {
  transform: translate(-50%, -50%) rotate(180deg);
}

.mx tr.gap > td {
  height: 6px;
  padding: 0;
}

.mx th.rh {
  width: 48px;
  min-width: 48px;
  max-width: 48px;
  height: 32px;
  padding: 0 4px 0 8px;
  text-align: left;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.mx th.rh b {
  font: var(--fontWeightSemibold) 11px / 14px var(--fontFamilyBase);
  color: var(--text-2);
}

.mx th.hl {
  background: var(--accent-soft);
  color: var(--text);
  border-radius: 4px;
}

.mx th.hl b,
.mx th.hl .col-code {
  color: var(--text);
}

.cell {
  display: block;
  width: 32px;
  height: 32px;
  padding: 0;
  border-radius: var(--r-ctl);
  border: 1px solid var(--stroke);
  background: transparent;
  color: var(--text-dis);
  font: 11px / 12px var(--fontFamilyBase);
  font-variant-numeric: tabular-nums;
  overflow: hidden;
  cursor: pointer;
  transition: background var(--dur-fast) linear;
}

.cell:hover:not(:disabled) {
  background: var(--card-hover);
  color: var(--text-2);
}

.cell.on {
  background: var(--accent);
  color: var(--on-accent);
  border-color: transparent;
}

.cell.on:hover {
  background: var(--colorBrandBackgroundHover);
  color: var(--on-accent);
}

.cell.off {
  border: 1px dashed var(--stroke-strong);
  color: var(--text-3);
}

.cell.sel {
  outline: 2px solid var(--text);
  outline-offset: 1px;
}

.cell:focus-visible {
  outline: 2px solid var(--text);
  outline-offset: 1px;
}

.cell:disabled {
  cursor: not-allowed;
  opacity: 0.35;
  color: transparent;
}

.cell.self {
  visibility: hidden;
}

/* 状态行和图例的文字不决定面板宽度 */
.mx-info,
.legend {
  contain: inline-size;
}

.mx-info {
  display: flex;
  align-items: center;
  gap: 6px;
  min-height: 34px;
  padding: 6px 16px;
  border-top: 1px solid var(--stroke);
  font-size: var(--fontSizeBase200);
  color: var(--text-3);
}

.mx-info.active {
  color: var(--text);
}

.mx-info svg {
  flex: none;
  width: 16px;
  height: 16px;
}

.legend {
  display: flex;
  flex-wrap: wrap;
  gap: 4px 12px;
  padding: 6px 16px 10px;
  font-size: 11px;
  color: var(--text-3);
  border-top: 1px solid var(--stroke);
}

.legend i {
  display: inline-block;
  width: 14px;
  height: 10px;
  margin-right: 4px;
  border-radius: 2px;
  vertical-align: -1px;
  border: 1px solid var(--stroke-strong);
}

.legend i.lg-on {
  background: var(--accent);
  border-color: transparent;
}

.legend i.lg-off {
  border-style: dashed;
}

/* 重复路由只警告：用警告色描边标出，不改变开关的颜色 */
.legend i.lg-dup,
.cell.duplicate {
  box-shadow: inset 0 0 0 2px var(--warn);
}
</style>
