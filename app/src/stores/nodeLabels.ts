// 节点的代号和副标题（工作台、路由矩阵、控制台、设备页共用）。
// 代号全部大写并带编号（维护者 2026-11-08）：外部输入 A1、A2…，外部输出 D1、D2…，总线 BUS1、BUS2…
// （都按节点 ID 顺序）；虚拟节点用所占槽位的编号 IN1、OUT2，与 Windows 中的设备名 "Aemeath In 1" 对应。

import type { NodeKind, NodeView, SlotRef } from '@/api/types'
import { i18n } from '@/i18n'

/** 按节点 ID 顺序编号的节点类型及其代号前缀 */
const NUMBERED_PREFIX: Partial<Record<NodeKind, string>> = {
  externalInput: 'A',
  externalOutput: 'D',
  bus: 'BUS',
}

export function slotCode(slot: SlotRef): string {
  return `${slot.direction === 'in' ? 'IN' : 'OUT'}${slot.number}`
}

/** 槽位的固定设备名，例如 "Aemeath In 1" */
export function slotName(slot: SlotRef): string {
  const { t } = i18n.global
  return t('node.slotName', {
    direction: t(slot.direction === 'in' ? 'node.directionIn' : 'node.directionOut'),
    number: slot.number,
  })
}

export function nodeCode(node: NodeView, all: readonly NodeView[]): string {
  if (node.slot !== null) {
    return slotCode(node.slot)
  }
  const prefix = NUMBERED_PREFIX[node.kind] ?? ''
  const sameKind = all.filter((other) => other.kind === node.kind).sort((a, b) => a.id - b.id)
  return `${prefix}${sameKind.findIndex((other) => other.id === node.id) + 1}`
}

/** 副标题："外部输入 · Rear Mic"、"虚拟输入 · Aemeath In 1" */
export function nodeSubtitle(node: NodeView): string {
  const { t } = i18n.global
  const detail = node.slot !== null ? slotName(node.slot) : (node.device?.name ?? '')
  return detail === '' ? t(`kind.${node.kind}`) : t('node.subtitle', { kind: t(`kind.${node.kind}`), detail })
}
