// 开发用的界面预览（只由开发服务器的 preview.html 加载，不进入发布构建）：
// 用 Tauri 的 mockIPC 模拟后端，便于在普通浏览器（Playwright 驱动的无头 Edge）里检查布局和交互。
// 这里的连线规则只是为了演示提示文字的简化实现；产品里的规则只在后端（aemeath_protocol::topology::check_route）。

import type { Channel } from '@tauri-apps/api/core'
import { emit } from '@tauri-apps/api/event'
import { mockIPC, mockWindows } from '@tauri-apps/api/mocks'
import type { Capabilities, FormatView, NodeView, RouteView, SlotView, ViewState } from '@/api/types'
import { DEFAULT_DEVICE_SETTINGS, roleOf } from '@/api/types'

const LONG_NAME = '直播推流混音输出 · 给 OBS 录制和推流使用的一个非常非常长的节点名称，用来测试截断效果'
const METER_INTERVAL_MS = 33
const INT32_48K: FormatView = { sampleRate: 48000, sampleFormat: 'int32', channels: 2 }

function node(partial: Partial<NodeView> & Pick<NodeView, 'id' | 'name' | 'kind' | 'x' | 'y'>): NodeView {
  const external =
    partial.kind === 'externalInput' || partial.kind === 'externalOutput' || partial.kind === 'externalInsert'
  return {
    slot: null,
    device: null,
    program: null,
    resampler: null,
    gainDb: 0,
    muted: false,
    gateDb: null,
    io: external ? { ...DEFAULT_DEVICE_SETTINGS } : null,
    format: external ? INT32_48K : null,
    candidates: [],
    status: { state: 'ok' },
    duplicate: null,
    recorder: null,
    muteHotkey: null,
    recordHotkey: null,
    ...partial,
  }
}

/** 模拟的设备能力：两个采样率 × 三种样本格式 */
const CAPABILITIES: Capabilities = {
  exclusiveFormats: [44100, 48000].flatMap((sampleRate) =>
    (['int16', 'int24In32', 'int32'] as const).map((sampleFormat) => ({ sampleRate, sampleFormat, channels: 2 })),
  ),
  recommended: INT32_48K,
  sharedFormat: { sampleRate: 48000, sampleFormat: 'float32', channels: 2 },
  defaultPeriodMs: 10,
  minimumPeriodMs: 2.667,
}

const nodes: NodeView[] = [
  node({
    id: 1,
    name: '麦克风 · Rear Mic',
    kind: 'externalInput',
    device: { endpointId: 'mic', name: 'Rear Mic (Realtek USB Audio)' },
    gateDb: -45,
    x: 40,
    y: 60,
  }),
  node({ id: 2, name: '游戏与浏览器', kind: 'virtualInput', slot: { direction: 'in', number: 1 }, x: 40, y: 260 }),
  node({ id: 6, name: '直播总线', kind: 'bus', x: 260, y: 160 }),
  node({ id: 3, name: LONG_NAME, kind: 'virtualOutput', slot: { direction: 'out', number: 1 }, x: 480, y: 40 }),
  node({
    id: 4,
    name: '监听音箱 · LG ULTRAGEAR+',
    kind: 'externalOutput',
    device: { endpointId: 'lg', name: 'LG ULTRAGEAR+ (NVIDIA High Definition Audio)' },
    x: 480,
    y: 280,
  }),
  node({
    id: 5,
    name: 'NVIDIA Broadcast',
    kind: 'externalInput',
    device: { endpointId: 'nv', name: 'Microphone (NVIDIA Broadcast)' },
    status: { state: 'offline', reason: 'inUseByOtherApp' },
    format: null,
    x: 40,
    y: 420,
  }),
  node({
    id: 10,
    name: 'NVIDIA Broadcast 降噪',
    kind: 'externalInsert',
    slot: { direction: 'out', number: 3 },
    device: { endpointId: 'nvb', name: 'Microphone (NVIDIA Broadcast)' },
    x: 260,
    y: 320,
  }),
  node({
    id: 11,
    name: '直播录音',
    kind: 'recorder',
    recorder: {
      settings: { format: 'flac', depth: 'int24' },
      state: {
        state: 'recording',
        file: 'C:\\Users\\me\\Music\\Aemeath\\直播录音_2026-11-08_20-00-00.flac',
        frames: 3_456_000,
        bytes: 18_874_368,
      },
    },
    x: 480,
    y: 420,
  }),
  node({
    id: 12,
    name: '备份录音',
    kind: 'recorder',
    recorder: { settings: { format: 'wav', depth: 'float32' }, state: { state: 'failed', reason: 'diskFull' } },
    x: 480,
    y: 540,
  }),
  node({
    id: 7,
    name: 'Google Chrome',
    duplicate: { program: 'Google Chrome', destination: '监听音箱' },
    kind: 'appSource',
    program: { path: 'C:\\Program Files\\Google\\Chrome\\chrome.exe', fileName: 'chrome.exe' },
    x: 40,
    y: 560,
  }),
  node({
    id: 8,
    name: 'cloudmusic',
    kind: 'appSource',
    program: { path: 'G:\\CloudMusic\\cloudmusic.exe', fileName: 'cloudmusic.exe' },
    status: { state: 'notRunning' },
    x: 40,
    y: 660,
  }),
  node({
    id: 9,
    name: '已卸载的播放器',
    kind: 'appSource',
    program: { path: 'D:\\Old\\player.exe', fileName: 'player.exe' },
    status: { state: 'programMissing' },
    x: 40,
    y: 760,
  }),
]

let state: ViewState = {
  engine: { state: 'running', sampleRate: 48000 },
  registration: null,
  canUndo: false,
  usbipVersion: '0.9.8.1',
  language: { mode: 'system' },
  lastPage: null,
  nodes,
  routes: [
    { from: 1, to: 3, gainDb: -3, enabled: true, labelPosition: 0.5, duplicate: null, toggleHotkey: null },
    {
      from: 2,
      to: 4,
      gainDb: 0,
      enabled: true,
      labelPosition: 0.5,
      duplicate: { program: 'Google Chrome', destination: '监听音箱' },
      toggleHotkey: null,
    },
    { from: 1, to: 4, gainDb: -6, enabled: false, labelPosition: 0.35, duplicate: null, toggleHotkey: null },
  ],
  console: [1, 2, 7, 6, 10, 3, 11],
  hotkeys: [
    {
      target: { type: 'panicMute' },
      binding: {
        key: { modifiers: { ctrl: true, alt: true, shift: false, win: false }, key: { type: 'keyboard', vk: 0x4d } },
        mode: 'toggle',
      },
      occupied: false,
    },
    {
      target: { type: 'nodeMute', node: 1 },
      binding: {
        key: { modifiers: { ctrl: false, alt: false, shift: false, win: false }, key: { type: 'mouseX1' } },
        mode: 'hold',
      },
      occupied: false,
    },
    {
      target: { type: 'routeToggle', from: 1, to: 3 },
      binding: {
        key: { modifiers: { ctrl: true, alt: false, shift: false, win: false }, key: { type: 'keyboard', vk: 0x7c } },
        mode: 'toggle',
      },
      occupied: true,
    },
  ],
  panicMute: true,
  recording: { folder: 'C:\\Users\\me\\Music\\Aemeath', defaults: { format: 'flac', depth: 'int24' } },
  notices: [],
  slots: [],
  duplicates: [{ program: 'Google Chrome', destination: '监听音箱' }],
}
const undoStack: ViewState[] = []

function slotsOf(current: ViewState): SlotView[] {
  const rows: SlotView[] = []
  for (const direction of ['in', 'out'] as const) {
    for (let number = 1; number <= 8; number++) {
      const occupant = current.nodes.find((n) => n.slot?.direction === direction && n.slot.number === number)
      const repairing = direction === 'out' && number === 2 && occupant !== undefined
      rows.push({
        slot: { direction, number },
        deviceName: `${direction === 'in' ? '扬声器' : '线路'} (Aemeath Dev ${direction === 'in' ? 'In' : 'Out'} ${number})`,
        occupant: occupant?.id ?? null,
        health: occupant
          ? repairing
            ? { state: 'retrying', failures: 1 }
            : { state: 'healthy' }
          : { state: 'unplugged' },
        format: { sampleRate: number > 2 && direction === 'out' ? 44100 : 48000, sampleFormat: 'int32', channels: 2 },
      })
    }
  }
  return rows
}

function commit(next: ViewState, undoable: boolean): ViewState {
  if (undoable) {
    undoStack.push(state)
  }
  state = { ...next, canUndo: undoStack.length > 0 }
  state.slots = slotsOf(state)
  return state
}

function reaches(routes: RouteView[], start: number, goal: number): boolean {
  const stack = [start]
  const seen = new Set(stack)
  while (stack.length > 0) {
    const current = stack.pop()
    if (current === goal) {
      return true
    }
    for (const route of routes.filter((r) => r.from === current)) {
      if (!seen.has(route.to)) {
        seen.add(route.to)
        stack.push(route.to)
      }
    }
  }
  return false
}

function check(from: number, to: number): string | null {
  const source = state.nodes.find((n) => n.id === from)
  const target = state.nodes.find((n) => n.id === to)
  if (source === undefined || target === undefined || from === to) {
    return 'selfRoute'
  }
  if (roleOf(source.kind) === 'out') {
    return 'cannotSend'
  }
  if (roleOf(target.kind) === 'in') {
    return 'cannotReceive'
  }
  if (state.routes.some((r) => r.from === from && r.to === to)) {
    return 'duplicate'
  }
  return reaches(state.routes, to, from) ? 'loop' : null
}

type Args = Record<string, unknown>

function updateRoute(args: Args, change: Partial<RouteView>, undoable: boolean): ViewState {
  const routes = state.routes.map((r) => (r.from === args.from && r.to === args.to ? { ...r, ...change } : r))
  return commit({ ...state, routes }, undoable)
}

function updateNode(args: Args, change: Partial<NodeView>, undoable: boolean): ViewState {
  const nodes = state.nodes.map((n) => (n.id === args.node ? { ...n, ...change } : n))
  return commit({ ...state, nodes }, undoable)
}

state = commit(state, false)
mockWindows('main')
mockIPC(
  (cmd, payload) => {
    const args = (payload ?? {}) as Args
    switch (cmd) {
      case 'get_state':
        return state
      case 'route_verdicts':
        return state.nodes.map((n) => ({
          node: n.id,
          rejection: args.fromOutput ? check(args.node as number, n.id) : check(n.id, args.node as number),
        }))
      case 'connect':
        return commit(
          {
            ...state,
            routes: [
              ...state.routes,
              {
                from: args.from as number,
                to: args.to as number,
                gainDb: 0,
                enabled: true,
                labelPosition: 0.5,
                duplicate: null,
                toggleHotkey: null,
              },
            ],
          },
          true,
        )
      case 'disconnect':
        return commit(
          { ...state, routes: state.routes.filter((r) => !(r.from === args.from && r.to === args.to)) },
          true,
        )
      case 'set_route_enabled':
        return updateRoute(args, { enabled: args.enabled as boolean }, true)
      case 'set_route_gain':
        return updateRoute(args, { gainDb: args.gainDb as number }, false)
      case 'set_route_label':
        return updateRoute(args, { labelPosition: args.position as number }, false)
      case 'move_node':
        return commit(
          {
            ...state,
            nodes: state.nodes.map((n) =>
              n.id === args.node ? { ...n, x: args.x as number, y: args.y as number } : n,
            ),
          },
          false,
        )
      case 'list_apps':
        return {
          apps: [
            {
              processId: 101,
              name: 'cloudmusic',
              executable: 'G:\\CloudMusic\\cloudmusic.exe',
              landing: 'in1',
              assigned: 'in1',
              system: false,
              sourceNode: null,
            },
            {
              processId: 102,
              name: 'game',
              executable: 'D:\\Games\\game.exe',
              landing: 'lg',
              assigned: 'in1',
              system: false,
              sourceNode: null,
            },
            {
              processId: 103,
              name: LONG_NAME,
              executable: 'C:\\Program Files\\Very Long Vendor\\app.exe',
              landing: 'vm',
              assigned: null,
              system: false,
              sourceNode: null,
            },
            { processId: 0, name: '', executable: null, landing: 'lg', assigned: null, system: true, sourceNode: null },
          ],
          choices: [
            {
              endpointId: 'in1',
              kind: 'virtualInput',
              name: '游戏与浏览器',
              detail: '扬声器 (Aemeath Dev In 1)',
              exclusiveByAemeath: false,
            },
            {
              endpointId: 'lg',
              kind: 'externalOutput',
              name: 'LG ULTRAGEAR+ (NVIDIA High Definition Audio)',
              detail: null,
              exclusiveByAemeath: true,
            },
            {
              endpointId: 'rdo',
              kind: 'externalOutput',
              name: 'Realtek Digital Output (Realtek USB Audio)',
              detail: null,
              exclusiveByAemeath: false,
            },
            {
              endpointId: 'vm',
              kind: 'other',
              name: 'Voicemeeter Input (VB-Audio Voicemeeter VAIO)',
              detail: null,
              exclusiveByAemeath: false,
            },
          ],
        }
      case 'set_landing':
        return null
      case 'landing_count':
        return 0
      case 'set_hotkey': {
        const target = args.target as { type: string; node?: number }
        return target.type === 'nodeMute'
          ? updateNode({ node: target.node }, { muteHotkey: args.binding as NodeView['muteHotkey'] }, false)
          : state
      }
      case 'set_panic_mute':
        return commit({ ...state, panicMute: args.active as boolean }, false)
      case 'start_recording':
      case 'stop_recording':
      case 'open_recording_folder':
        return null
      case 'set_recording_folder':
        return commit({ ...state, recording: { ...state.recording, folder: args.folder as string } }, false)
      case 'set_recording_defaults':
        return commit(
          {
            ...state,
            recording: { ...state.recording, defaults: args.settings as ViewState['recording']['defaults'] },
          },
          false,
        )
      case 'describe_program':
        return (
          (args.path as string)
            .split('\\')
            .pop()
            ?.replace(/\.exe$/i, '') ?? ''
        )
      case 'plugin:dialog|open':
        return 'D:\\Games\\new-game.exe'
      case 'list_default_devices':
        return {
          render: [
            {
              endpointId: 'lg',
              name: 'LG ULTRAGEAR+ (NVIDIA High Definition Audio)',
              isDefault: false,
              isCommunications: false,
              exclusiveByAemeath: true,
            },
            {
              endpointId: 'in1',
              name: '扬声器 (Aemeath Dev In 1)',
              isDefault: true,
              isCommunications: false,
              exclusiveByAemeath: false,
            },
            {
              endpointId: 'rdo',
              name: 'Realtek Digital Output (Realtek USB Audio)',
              isDefault: false,
              isCommunications: true,
              exclusiveByAemeath: false,
            },
          ],
          capture: [
            {
              endpointId: 'mic',
              name: 'Rear Mic (Realtek USB Audio)',
              isDefault: true,
              isCommunications: true,
              exclusiveByAemeath: true,
            },
            {
              endpointId: 'out1',
              name: '线路 (Aemeath Dev Out 1)',
              isDefault: false,
              isCommunications: false,
              exclusiveByAemeath: false,
            },
          ],
        }
      case 'set_default_device':
        return null
      case 'probe_device':
        return CAPABILITIES
      case 'set_device_settings':
        return updateNode(args, { io: args.settings as NodeView['io'] }, true)
      case 'redetect':
        return null
      case 'matrix_verdicts':
        return state.nodes.flatMap((from) =>
          state.nodes.flatMap((to) => {
            const rejection = check(from.id, to.id)
            return rejection === null || rejection === 'duplicate' || roleOf(from.kind) === 'out'
              ? []
              : roleOf(to.kind) === 'in'
                ? []
                : [{ from: from.id, to: to.id, rejection }]
          }),
        )
      case 'pin_node':
        return commit(
          {
            ...state,
            console: args.pinned
              ? [...state.console, args.node as number]
              : state.console.filter((id) => id !== args.node),
          },
          false,
        )
      case 'move_pin': {
        const rest = state.console.filter((id) => id !== args.node)
        rest.splice(args.index as number, 0, args.node as number)
        return commit({ ...state, console: rest }, false)
      }
      case 'change_slot':
        return updateNode(
          args,
          {
            slot: {
              direction: state.nodes.find((n) => n.id === args.node)?.slot?.direction ?? 'in',
              number: args.number as number,
            },
          },
          true,
        )
      case 'set_node_gain':
        return updateNode(args, { gainDb: args.gainDb as number }, false)
      case 'set_node_muted':
        return updateNode(args, { muted: args.muted as boolean }, true)
      case 'set_node_gate':
        return updateNode(args, { gateDb: args.thresholdDb as number | null }, false)
      case 'rename_node':
        return commit(
          { ...state, nodes: state.nodes.map((n) => (n.id === args.node ? { ...n, name: args.name as string } : n)) },
          true,
        )
      case 'remove_node':
        return commit(
          {
            ...state,
            nodes: state.nodes.filter((n) => n.id !== args.node),
            routes: state.routes.filter((r) => r.from !== args.node && r.to !== args.node),
            console: state.console.filter((id) => id !== args.node),
          },
          true,
        )
      case 'undo': {
        const previous = undoStack.pop()
        return previous === undefined ? state : commit(previous, false)
      }
      case 'set_last_page':
        return commit({ ...state, lastPage: args.page as string }, false)
      case 'set_language':
        return commit({ ...state, language: args.language as ViewState['language'] }, false)
      case 'list_devices':
        return args.direction === 'input'
          ? [{ endpointId: 'mic', name: 'Rear Mic (Realtek USB Audio)' }]
          : [
              { endpointId: 'lg', name: 'LG ULTRAGEAR+ (NVIDIA High Definition Audio)' },
              { endpointId: 'rdo', name: 'Realtek Digital Output (Realtek USB Audio)' },
            ]
      case 'watch_meters': {
        const channel = args.channel as Channel<ArrayBuffer>
        let phase = 0
        window.setInterval(() => {
          phase += 0.15
          const buffer = new ArrayBuffer(state.nodes.length * 12)
          const view = new DataView(buffer)
          state.nodes.forEach((n, index) => {
            const level = n.status.state === 'ok' ? 0.05 + 0.4 * Math.abs(Math.sin(phase + index)) : 0
            view.setUint32(index * 12, n.id, true)
            view.setFloat32(index * 12 + 4, level, true)
            view.setFloat32(index * 12 + 8, level * 0.8, true)
          })
          channel.onmessage(buffer)
        }, METER_INTERVAL_MS)
        return null
      }
      case 'plugin:window|is_maximized':
        return false
      default:
        return null
    }
  },
  { shouldMockEvents: true },
)

// 让预览也能测试"状态推送"：每 5 秒推一次当前状态
window.setInterval(() => void emit('state-changed', state), 5000)

await import('@/main')
