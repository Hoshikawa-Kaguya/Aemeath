// 节点的代号和副标题（工作台、设备页共用）。
// 代号沿用原型：外部输入 A1、A2…，外部输出 D1、D2…（按节点 ID 顺序），虚拟节点用槽位 In1、Out2。

import type { NodeView, SlotRef } from '@/api/types'
import { i18n } from '@/i18n'

const EXTERNAL_INPUT_PREFIX = 'A'
const EXTERNAL_OUTPUT_PREFIX = 'D'
const BUS_CODE = 'BUS'

export function slotCode(slot: SlotRef): string {
  return `${slot.direction === 'in' ? 'In' : 'Out'}${slot.number}`
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
  if (node.kind === 'bus') {
    return BUS_CODE
  }
  const prefix = node.kind === 'externalInput' ? EXTERNAL_INPUT_PREFIX : EXTERNAL_OUTPUT_PREFIX
  const sameKind = all.filter((other) => other.kind === node.kind).sort((a, b) => a.id - b.id)
  return `${prefix}${sameKind.findIndex((other) => other.id === node.id) + 1}`
}

/** 副标题："外部输入 · Rear Mic"、"虚拟输入 · Aemeath In 1" */
export function nodeSubtitle(node: NodeView): string {
  const { t } = i18n.global
  const detail = node.slot !== null ? slotName(node.slot) : (node.device?.name ?? '')
  return detail === '' ? t(`kind.${node.kind}`) : t('node.subtitle', { kind: t(`kind.${node.kind}`), detail })
}
