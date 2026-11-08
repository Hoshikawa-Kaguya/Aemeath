// 后端状态快照的类型，与 src-tauri/src/view.rs 一一对应（字段名为 camelCase）。

export type EngineState = 'connecting' | 'running' | 'unresponsive' | 'versionMismatch'

export type Direction = 'in' | 'out'

export interface SlotRef {
  direction: Direction
  number: number
}

export type NodeKind = 'externalInput' | 'virtualInput' | 'virtualOutput' | 'externalOutput' | 'bus'

export type OfflineReason = 'waitingForDevice' | 'inUseByOtherApp' | 'noSignal' | 'slotRepairing' | 'slotFailing'

export type NodeStatus = { state: 'ok' } | { state: 'pending' } | { state: 'offline'; reason: OfflineReason }

export interface DeviceView {
  endpointId: string
  name: string
}

export interface NodeView {
  id: number
  name: string
  kind: NodeKind
  slot: SlotRef | null
  device: DeviceView | null
  x: number
  y: number
  gainDb: number
  muted: boolean
  status: NodeStatus
}

export interface RouteView {
  from: number
  to: number
  gainDb: number
  enabled: boolean
  /** 标签在连线上的位置，0.1–0.9 */
  labelPosition: number
}

export type SlotHealth =
  | { state: 'unplugged' }
  | { state: 'engineOffline' }
  | { state: 'settling' }
  | { state: 'healthy' }
  | { state: 'retrying'; failures: number }
  | { state: 'failing'; failures: number }

export type SampleFormat = 'int16' | 'int24' | 'int24In32' | 'int32' | 'float32' | 'float64'

export interface FormatView {
  sampleRate: number
  sampleFormat: SampleFormat
  channels: number
}

export interface SlotView {
  slot: SlotRef
  deviceName: string | null
  occupant: number | null
  health: SlotHealth
  format: FormatView | null
}

export interface RegistrationView {
  completed: number
  total: number
  failing: SlotRef[]
}

export type LanguagePreference = { mode: 'system' } | { mode: 'fixed'; locale: string }

export interface ViewState {
  engine: { state: EngineState; sampleRate: number }
  registration: RegistrationView | null
  firstRun: boolean
  canUndo: boolean
  usbipVersion: string | null
  language: LanguagePreference
  nodes: NodeView[]
  routes: RouteView[]
  slots: SlotView[]
}

export type Rejection = 'selfRoute' | 'cannotSend' | 'cannotReceive' | 'duplicate' | 'loop'

export interface RouteVerdict {
  node: number
  rejection: Rejection | null
}

export type NewNodeKind =
  | { type: 'externalInput'; endpointId: string; deviceName: string }
  | { type: 'externalOutput'; endpointId: string; deviceName: string }
  | { type: 'virtualInput' }
  | { type: 'virtualOutput' }

export interface DeviceChoice {
  endpointId: string
  name: string
}

/** 命令失败的原因（src-tauri/src/controller.rs 的 CommandError） */
export type CommandError =
  | { code: 'unavailable' }
  | { code: 'invalidName' }
  | { code: 'routeRejected'; rejection: Rejection }
  | { code: 'noFreeSlot' }
  | { code: 'slotOccupied' }
  | { code: 'deviceInUse' }
  | { code: 'notFound' }
  | { code: 'failed'; message: string }

/** 节点在信号流中的角色（决定颜色和有哪些端口） */
export type Role = 'in' | 'proc' | 'out'

export function roleOf(kind: NodeKind): Role {
  switch (kind) {
    case 'externalInput':
    case 'virtualInput':
      return 'in'
    case 'bus':
      return 'proc'
    case 'virtualOutput':
    case 'externalOutput':
      return 'out'
  }
}
