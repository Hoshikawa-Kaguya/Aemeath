// 工作台页面提供给节点和连线组件的操作（provide / inject），避免把回调塞进 Vue Flow 的 data。

import type { InjectionKey } from 'vue'
import type { NodeView, RouteView } from '@/api/types'
import type { Point } from './curve'

export interface WorkbenchActions {
  selectRoute(route: RouteView): void
  selectNode(node: NodeView): void
  toggleRoute(route: RouteView): void
  /** 滚轮调增益：每格 ±1 dB */
  nudgeGain(route: RouteView, steps: number): void
  /** 沿连线移动标签：`position` 是标签在连线上的位置（0–1） */
  moveLabel(route: RouteView, position: number): void
  rename(node: NodeView, name: string): void
  /** 改名结束（提交或取消），清除"正在改名"状态 */
  renameDone(): void
  /** 节点的右键菜单，也由节点上的"⋯"按钮和菜单键打开 */
  openNodeMenu(event: MouseEvent | KeyboardEvent, node: NodeView): void
  openRouteMenu(event: MouseEvent | KeyboardEvent, route: RouteView): void
  /** 打开节点设置对话框 */
  openSettings(node: NodeView): void
  /** 屏幕坐标转成工作台坐标（拖动标签时用） */
  toFlow(point: Point): Point
}

export const workbenchKey: InjectionKey<WorkbenchActions> = Symbol('workbench')

/** Vue Flow 节点的 data */
export interface NodeData {
  node: NodeView
  code: string
  subtitle: string
  selected: boolean
  /** 拖线经过时的校验结果：可以连（ok）、不能连（bad） */
  drop: 'ok' | 'bad' | null
  /** 从右键菜单选了"改名"：节点进入就地改名 */
  renaming: boolean
  /** 路由矩阵里悬停的格子以它为一端 */
  highlighted: boolean
}

/** Vue Flow 连线的 data */
export interface RouteData {
  route: RouteView
  sourceName: string
  targetName: string
  selected: boolean
  /** 路由矩阵里悬停的正是这条连线 */
  highlighted: boolean
}

/** 界面上增益的调节范围。后端允许 −120 到 +24 dB，界面只开放常用的一段，推子才不会太灵敏 */
export const GAIN_MIN_DB = -60
export const GAIN_MAX_DB = 12
