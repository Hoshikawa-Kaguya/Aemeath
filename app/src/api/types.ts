// 后端状态快照的类型，与 src-tauri/src/view.rs 一一对应（字段名为 camelCase）。

export type EngineState = 'connecting' | 'running' | 'unresponsive' | 'versionMismatch'

export type Direction = 'in' | 'out'

export interface SlotRef {
  direction: Direction
  number: number
}

export type NodeKind =
  | 'externalInput'
  | 'virtualInput'
  | 'virtualOutput'
  | 'externalOutput'
  | 'bus'
  | 'appSource'
  | 'externalInsert'
  | 'recorder'

export type OfflineReason =
  'waitingForDevice' | 'inUseByOtherApp' | 'exclusiveNotAllowed' | 'noSignal' | 'slotRepairing' | 'slotFailing'

/** 重采样算法及其重采样预设，src-tauri/src/device_settings.rs */
export type Resampler =
  | { algorithm: 'polyAsync'; preset: 'fastest' | 'smooth' | 'fine' }
  | { algorithm: 'sincAsync'; preset: 'veryFast' | 'fast' | 'balanced' | 'accurate' | 'ultra' }
  | { algorithm: 'fftSync'; preset: 'lowLatency' | 'balanced' }

export type BufferChoice =
  { choice: 'deviceDefault' } | { choice: 'deviceMinimum' } | { choice: 'milliseconds'; ms: number }

/** 外部设备节点的设备设置，src-tauri/src/device_settings.rs */
export interface DeviceSettings {
  exclusive: boolean
  /** 独占时的格式；null 表示推荐格式 */
  format: FormatView | null
  buffer: BufferChoice
  /** 按节点覆盖的重采样算法；null 表示跟随全局默认 */
  resampler: Resampler | null
}

/** 占用槽位的节点的设置，src-tauri/src/device_settings.rs */
export interface SlotSettings {
  /** null 表示跟随引擎 */
  format: FormatView | null
  /** null 表示跟随全局默认 */
  resampler: Resampler | null
}

export const DEFAULT_SLOT_SETTINGS: SlotSettings = { format: null, resampler: null }

/** 设备能力（添加节点、节点设置里的格式和缓冲选项） */
export interface Capabilities {
  exclusiveFormats: FormatView[]
  recommended: FormatView | null
  sharedFormat: FormatView | null
  defaultPeriodMs: number
  minimumPeriodMs: number
}

export const DEFAULT_DEVICE_SETTINGS: DeviceSettings = {
  exclusive: true,
  format: null,
  buffer: { choice: 'deviceDefault' },
  resampler: null,
}

export type NodeStatus =
  | { state: 'ok' }
  | { state: 'pending' }
  | { state: 'offline'; reason: OfflineReason }
  | { state: 'notRunning' }
  | { state: 'programMissing' }

export interface DeviceView {
  endpointId: string
  name: string
}

/** 应用源绑定的程序 */
export interface ProgramView {
  path: string
  fileName: string
}

export interface NodeView {
  id: number
  name: string
  kind: NodeKind
  slot: SlotRef | null
  device: DeviceView | null
  program: ProgramView | null
  /** 应用源按节点覆盖的重采样算法；null 表示跟随全局默认 */
  resampler: Resampler | null
  x: number
  y: number
  gainDb: number
  muted: boolean
  /** 门限阈值（dBFS）；null 表示关。只有输入节点会有 */
  gateDb: number | null
  /** 外部设备节点的设备设置 */
  io: DeviceSettings | null
  /** 占用槽位的节点的格式与重采样算法 */
  slotSettings: SlotSettings | null
  /** 外部设备在线时实际使用的设备格式 */
  format: FormatView | null
  /** 设备换了端点、又有多个同名设备时，可以改用的候选 */
  candidates: DeviceView[]
  status: NodeStatus
  /** 录音节点的设置与状态 */
  recorder: RecorderView | null
  muteHotkey: HotkeyBindingView | null
  recordHotkey: HotkeyBindingView | null
  /** 这个应用源造成了重复路由 */
  duplicate: DuplicateView | null
}

/** 应用页（src-tauri/src/apps.rs） */
export interface AppView {
  processId: number
  /** 程序名；系统声音为空 */
  name: string
  executable: string | null
  /** 会话当前所在的端点（实际落点） */
  landing: string
  /** Aemeath 记录的指派 */
  assigned: string | null
  system: boolean
  /** 这个程序已有的应用源节点 */
  sourceNode: number | null
}

export interface LandingChoice {
  endpointId: string
  kind: 'virtualInput' | 'externalOutput' | 'other'
  name: string
  detail: string | null
  exclusiveByAemeath: boolean
}

export interface AppsView {
  apps: AppView[]
  choices: LandingChoice[]
}

/** 工具页的系统默认设备（src-tauri/src/default_devices.rs） */
export interface DefaultDeviceView {
  endpointId: string
  name: string
  isDefault: boolean
  isCommunications: boolean
  exclusiveByAemeath: boolean
}

export interface DefaultDevicesView {
  render: DefaultDeviceView[]
  capture: DefaultDeviceView[]
}

/** 后端给出的一次性提示（src-tauri/src/view.rs 的 NoticeView） */
export interface Notice {
  id: number
  kind:
    | { type: 'deviceReplaced'; node: string; device: string }
    | { type: 'landingRestored'; app: string; landing: string }
    | { type: 'recordingFailed'; node: string; reason: RecordingFailure }
}

export interface ModifiersView {
  ctrl: boolean
  alt: boolean
  shift: boolean
  win: boolean
}

export type KeyCodeView = { type: 'keyboard'; vk: number } | { type: 'mouseX1' } | { type: 'mouseX2' }

export interface HotkeyView {
  modifiers: ModifiersView
  key: KeyCodeView
}

export type HotkeyModeView = 'toggle' | 'hold'

export interface HotkeyBindingView {
  key: HotkeyView
  mode: HotkeyModeView
}

export type HotkeyTargetView =
  | { type: 'nodeMute'; node: number }
  | { type: 'routeToggle'; from: number; to: number }
  | { type: 'nodeRecord'; node: number }
  | { type: 'allRecording' }
  | { type: 'panicMute' }
  | { type: 'toggleWindow' }

export interface HotkeyRowView {
  target: HotkeyTargetView
  binding: HotkeyBindingView
  /** 被其他程序占用 */
  occupied: boolean
}

export type RecordFormat = 'wav' | 'flac'
/** FLAC 编码器支持的最高采样率：引擎更高时录音文件会降采样（recording_file.rs） */
export const FLAC_MAX_HZ = 96000
export type RecordDepth = 'int16' | 'int24' | 'float32'

export interface RecorderSettingsView {
  format: RecordFormat
  depth: RecordDepth
}

export type RecordingFailure = 'diskFull' | 'writeFailed'

export type RecorderStateView =
  | { state: 'idle' }
  | { state: 'recording'; file: string; frames: number; bytes: number }
  | { state: 'failed'; reason: RecordingFailure }

export interface RecorderView {
  settings: RecorderSettingsView
  state: RecorderStateView
}

export interface RecordingView {
  folder: string
  defaults: RecorderSettingsView
}

/** 一处重复路由的说明（只警告不阻止） */
export interface DuplicateView {
  program: string
  destination: string
}

export interface RouteView {
  from: number
  to: number
  gainDb: number
  enabled: boolean
  /** 标签在连线上的位置，0.1–0.9 */
  labelPosition: number
  /** 工作台上是否显示连线标签，默认不显示 */
  labelVisible: boolean
  /** 这条连线在重复路径上 */
  duplicate: DuplicateView | null
  toggleHotkey: HotkeyBindingView | null
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

/** 更新状态，src-tauri/src/updates.rs */
export type UpdateView = {
  currentVersion: string
  /** 打包运行才有更新功能 */
  supported: boolean
  portable: boolean
} & (
  | { state: 'unchecked' | 'checking' | 'upToDate' | 'failed' }
  | { state: 'available' | 'ready'; version: string }
  | { state: 'downloading'; version: string; percent: number }
)

export type ThemeMode = 'system' | 'dark' | 'light'

/** 界面外观，src-tauri/src/view.rs */
export interface Appearance {
  themeMode: ThemeMode
  /** `#rrggbb`；null 表示默认 */
  accent: string | null
  /** 背景图的完整路径；null 表示没有 */
  backgroundPath: string | null
  overlayPercent: number
}

/** 标题栏 CPU、ASRC 标签的数据；百分比和警告由后端算好 */
export interface EngineStats {
  cpuPercent: number
  realtimePeakPercent: number
  realtimeWarn: boolean
  /** 没有任何 ASRC 缓冲时为 null；`percent` 是偏离 100% 最远的那个缓冲的占用比 */
  asrc: {
    percent: number
    warn: boolean
    buffers: { node: number; levelMs: number; targetMs: number; percent: number }[]
  } | null
}

/** 启动时配置文件的问题。前两种只读（这次的修改不保存）；`backup` 只是文件名 */
export type ConfigStatus =
  | { type: 'newerVersion'; found: number }
  | { type: 'unreadable'; message: string }
  | { type: 'recovered'; backup: string }

export interface ViewState {
  /** `clockMaster` 是主时钟节点的 ID；null 表示按系统时钟运行。`stats` 是最近一次性能统计，引擎没有上报时为 null */
  engine: {
    state: EngineState
    sampleRate: number
    resampler: Resampler
    clockMaster: number | null
    stats: EngineStats | null
  }
  registration: RegistrationView | null
  /** 启动时配置文件正常时为 null，本次运行内不变 */
  configStatus: ConfigStatus | null
  usbipVersion: string | null
  /** 驱动版本比 Aemeath 附带的新，没有经过测试 */
  usbipUntested: boolean
  language: LanguagePreference
  checkUpdatesOnStart: boolean
  appearance: Appearance
  /** 上次停留的页面；没有记录时为 null */
  lastPage: string | null
  nodes: NodeView[]
  routes: RouteView[]
  /** 固定到控制台的节点 ID，从左到右 */
  console: number[]
  recording: RecordingView
  hotkeys: HotkeyRowView[]
  /** 紧急静音开启中 */
  panicMute: boolean
  slots: SlotView[]
  notices: Notice[]
  duplicates: DuplicateView[]
}

export type Rejection = 'selfRoute' | 'cannotSend' | 'cannotReceive' | 'duplicate' | 'loop'

export interface RouteVerdict {
  node: number
  rejection: Rejection | null
}

/** 路由矩阵里不能新建连线的格子 */
export interface PairVerdict {
  from: number
  to: number
  rejection: Rejection
}

export type NewNodeKind =
  | { type: 'externalInput'; endpointId: string; deviceName: string }
  | { type: 'externalOutput'; endpointId: string; deviceName: string }
  | { type: 'virtualInput' }
  | { type: 'virtualOutput' }
  | { type: 'bus' }
  | { type: 'appSource'; program: string; resampler: Resampler | null }
  | { type: 'recorder' }
  | { type: 'externalInsert'; endpointId: string; deviceName: string }

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
  | { code: 'programInUse' }
  | { code: 'invalidProgram' }
  | { code: 'hotkeyModifierRequired' }
  | { code: 'hotkeyToggleOnly' }
  | { code: 'hotkeyConflict'; target: HotkeyTargetView }
  | { code: 'notFound' }
  | { code: 'failed'; message: string }

/** 节点在信号流中的角色（决定颜色和有哪些端口） */
export type Role = 'in' | 'proc' | 'out'

/** 与后端 `NodeKind::role` 的划分一致 */
export function roleOf(kind: NodeKind): Role {
  switch (kind) {
    case 'externalInput':
    case 'virtualInput':
    case 'appSource':
      return 'in'
    case 'bus':
    case 'externalInsert':
      return 'proc'
    case 'virtualOutput':
    case 'externalOutput':
    case 'recorder':
      return 'out'
  }
}
