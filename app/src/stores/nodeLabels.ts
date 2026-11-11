// 节点的代号和副标题（工作台、路由矩阵、控制台、设备页共用）。
// 代号全部大写并带编号：外部输入 A1、A2…，外部输出 D1、D2…，总线 BUS1、BUS2…
// 应用源 APP1、APP2…（都按节点 ID 顺序）；虚拟节点用所占槽位的编号 IN1、OUT2，与 Windows 中的设备名 "Aemeath In 1" 对应。

import type { NodeKind, NodeView, SlotRef } from '@/api/types'
import { i18n } from '@/i18n'

/** 按节点 ID 顺序编号的节点类型及其代号前缀 */
const NUMBERED_PREFIX: Partial<Record<NodeKind, string>> = {
  externalInput: 'A',
  externalOutput: 'D',
  bus: 'BUS',
  appSource: 'APP',
  externalInsert: 'FX',
  recorder: 'REC',
}

/** 完整路径里的文件名 */
export function fileNameOf(path: string): string {
  return path.split(/[\\/]/).pop() ?? path
}

/** 槽位的代号，例如 IN1、OUT2 */
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

/** 节点的代号：占槽位的用槽位代号，其余按同类节点的 ID 顺序编号。`all` 是工作台上的全部节点 */
export function nodeCode(node: NodeView, all: readonly NodeView[]): string {
  // 外部插入虽然占用槽位，节点上不显示槽位
  if (node.slot !== null && node.kind !== 'externalInsert') {
    return slotCode(node.slot)
  }
  const prefix = NUMBERED_PREFIX[node.kind] ?? ''
  const sameKind = all.filter((other) => other.kind === node.kind).sort((a, b) => a.id - b.id)
  return `${prefix}${sameKind.findIndex((other) => other.id === node.id) + 1}`
}

/** 副标题："外部输入 · Rear Mic"、"虚拟输入 · Aemeath In 1" */
export function nodeSubtitle(node: NodeView): string {
  const { t } = i18n.global
  if (node.kind === 'externalInsert') {
    return t('kind.externalInsert')
  }
  const detail = node.slot !== null ? slotName(node.slot) : (node.device?.name ?? node.program?.fileName ?? '')
  return detail === '' ? t(`kind.${node.kind}`) : t('node.subtitle', { kind: t(`kind.${node.kind}`), detail })
}
