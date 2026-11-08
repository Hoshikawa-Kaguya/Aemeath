// 后端命令与事件的封装。命令名、参数名与 src-tauri/src/commands.rs 对应（Tauri 自动把 camelCase 参数转成 snake_case）。

import { Channel, invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type { DeviceChoice, LanguagePreference, NewNodeKind, RouteVerdict, ViewState } from './types'

const STATE_EVENT = 'state-changed'

export const backend = {
  getState: () => invoke<ViewState>('get_state'),
  initialize: (defaultInputName: string) => invoke<ViewState>('initialize', { defaultInputName }),
  addNode: (kind: NewNodeKind, name: string, x: number, y: number) =>
    invoke<ViewState>('add_node', { kind, name, x, y }),
  removeNode: (node: number) => invoke<ViewState>('remove_node', { node }),
  renameNode: (node: number, name: string) => invoke<ViewState>('rename_node', { node, name }),
  moveNode: (node: number, x: number, y: number) => invoke<ViewState>('move_node', { node, x, y }),
  routeVerdicts: (node: number, fromOutput: boolean) => invoke<RouteVerdict[]>('route_verdicts', { node, fromOutput }),
  connect: (from: number, to: number) => invoke<ViewState>('connect', { from, to }),
  disconnect: (from: number, to: number) => invoke<ViewState>('disconnect', { from, to }),
  setRouteEnabled: (from: number, to: number, enabled: boolean) =>
    invoke<ViewState>('set_route_enabled', { from, to, enabled }),
  setRouteGain: (from: number, to: number, gainDb: number) => invoke<ViewState>('set_route_gain', { from, to, gainDb }),
  setRouteLabel: (from: number, to: number, position: number) =>
    invoke<ViewState>('set_route_label', { from, to, position }),
  undo: () => invoke<ViewState>('undo'),
  setLanguage: (language: LanguagePreference) => invoke<ViewState>('set_language', { language }),
  listDevices: (direction: 'input' | 'output') => invoke<DeviceChoice[]>('list_devices', { direction }),
  checkSlots: () => invoke<void>('check_slots'),
  setTrayLabels: (open: string, quit: string) => invoke<void>('set_tray_labels', { open, quit }),
}

export function onStateChanged(handler: (state: ViewState) => void): Promise<UnlistenFn> {
  return listen<ViewState>(STATE_EVENT, (event) => handler(event.payload))
}

/** 每个节点 12 字节：节点 ID（u32）、左声道峰值、右声道峰值（f32），小端（src-tauri/src/meters.rs） */
const BYTES_PER_NODE = 12
const LEFT_OFFSET = 4
const RIGHT_OFFSET = 8

export interface MeterSample {
  node: number
  left: number
  right: number
}

/** 订阅电平。后端约 30 Hz 推送一次自上次以来的峰值（线性幅度）。 */
export function watchMeters(handler: (samples: MeterSample[]) => void): Promise<void> {
  const channel = new Channel<ArrayBuffer>()
  channel.onmessage = (buffer) => {
    const view = new DataView(buffer)
    const samples: MeterSample[] = []
    for (let offset = 0; offset + BYTES_PER_NODE <= view.byteLength; offset += BYTES_PER_NODE) {
      samples.push({
        node: view.getUint32(offset, true),
        left: view.getFloat32(offset + LEFT_OFFSET, true),
        right: view.getFloat32(offset + RIGHT_OFFSET, true),
      })
    }
    handler(samples)
  }
  return invoke('watch_meters', { channel })
}
