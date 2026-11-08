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
  moveLabel(route: RouteView, position: number): void
  rename(node: NodeView, name: string): void
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
}

/** Vue Flow 连线的 data */
export interface RouteData {
  route: RouteView
  sourceName: string
  targetName: string
  selected: boolean
}

/** 增益的调节范围（与原型一致；后端允许 −120 到 +24 dB） */
export const GAIN_MIN_DB = -60
export const GAIN_MAX_DB = 12
