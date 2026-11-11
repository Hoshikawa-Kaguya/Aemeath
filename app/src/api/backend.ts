// 后端命令与事件的封装。命令名、参数名与 src-tauri/src/commands.rs 对应（Tauri 自动把 camelCase 参数转成 snake_case）。

import { Channel, invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type {
  Appearance,
  AppsView,
  Capabilities,
  DefaultDevicesView,
  DeviceChoice,
  DeviceSettings,
  HotkeyBindingView,
  HotkeyTargetView,
  LanguagePreference,
  NewNodeKind,
  PairVerdict,
  RecorderSettingsView,
  Resampler,
  RouteVerdict,
  SlotSettings,
  UpdateView,
  ViewState,
} from './types'

const STATE_EVENT = 'state-changed'
const UPDATE_EVENT = 'update-changed'

export const backend = {
  getState: () => invoke<ViewState>('get_state'),
  /** 预设：只在工作台为空时生效 */
  applyPreset: (inputName: string, outputName: string) => invoke<ViewState>('apply_preset', { inputName, outputName }),
  /** `io`、`slotSettings`：外部设备、占用槽位的节点在添加时就设定的设置；null 表示默认值 */
  addNode: (
    kind: NewNodeKind,
    name: string,
    x: number,
    y: number,
    io: DeviceSettings | null = null,
    slotSettings: SlotSettings | null = null,
  ) => invoke<ViewState>('add_node', { kind, name, x, y, io, slotSettings }),
  setSlotSettings: (node: number, settings: SlotSettings) => invoke<ViewState>('set_slot_settings', { node, settings }),
  setDeviceSettings: (node: number, settings: DeviceSettings) =>
    invoke<ViewState>('set_device_settings', { node, settings }),
  replaceDevice: (node: number, endpointId: string, deviceName: string) =>
    invoke<ViewState>('replace_device', { node, endpointId, deviceName }),
  /** 应用源的默认节点名（文件说明，或不带扩展名的文件名） */
  describeProgram: (path: string) => invoke<string>('describe_program', { path }),
  replaceProgram: (node: number, program: string) => invoke<ViewState>('replace_program', { node, program }),
  /** `resampler` 为 null 表示跟随全局默认 */
  setAppResampler: (node: number, resampler: Resampler | null) =>
    invoke<ViewState>('set_app_resampler', { node, resampler }),
  setGlobalResampler: (resampler: Resampler) => invoke<ViewState>('set_global_resampler', { resampler }),
  setEngineRate: (hz: number) => invoke<ViewState>('set_engine_rate', { hz }),
  /** 设备打不开时为 null */
  probeDevice: (endpointId: string, direction: 'input' | 'output') =>
    invoke<Capabilities | null>('probe_device', { endpointId, direction }),
  listApps: () => invoke<AppsView>('list_apps'),
  /** `endpointId` 为 null 表示跟随系统默认 */
  setLanding: (processId: number, executable: string | null, endpointId: string | null) =>
    invoke<void>('set_landing', { processId, executable, endpointId }),
  landingCount: (node: number) => invoke<number>('landing_count', { node }),
  /** `binding` 为 null 表示清除 */
  setHotkey: (target: HotkeyTargetView, binding: HotkeyBindingView | null) =>
    invoke<ViewState>('set_hotkey', { target, binding }),
  setHotkeysPaused: (paused: boolean) => invoke<void>('set_hotkeys_paused', { paused }),
  setPanicMute: (active: boolean) => invoke<ViewState>('set_panic_mute', { active }),
  startRecording: (node: number) => invoke<void>('start_recording', { node }),
  stopRecording: (node: number) => invoke<void>('stop_recording', { node }),
  setRecorderSettings: (node: number, settings: RecorderSettingsView) =>
    invoke<ViewState>('set_recorder_settings', { node, settings }),
  setRecordingFolder: (folder: string) => invoke<ViewState>('set_recording_folder', { folder }),
  setRecordingDefaults: (settings: RecorderSettingsView) => invoke<ViewState>('set_recording_defaults', { settings }),
  openRecordingFolder: () => invoke<void>('open_recording_folder'),
  getUpdateState: () => invoke<UpdateView>('get_update_state'),
  checkUpdates: () => invoke<void>('check_updates'),
  downloadUpdate: () => invoke<void>('download_update'),
  applyUpdate: () => invoke<void>('apply_update'),
  openReleasesPage: () => invoke<void>('open_releases_page'),
  setCheckUpdatesOnStart: (enabled: boolean) => invoke<ViewState>('set_check_updates_on_start', { enabled }),
  autostartEnabled: () => invoke<boolean>('autostart_enabled'),
  setAutostart: (enabled: boolean) => invoke<boolean>('set_autostart', { enabled }),
  crashDumpsEnabled: () => invoke<boolean>('crash_dumps_enabled'),
  enableCrashDumps: () => invoke<boolean>('enable_crash_dumps'),
  removeVirtualDevices: () => invoke<void>('remove_virtual_devices'),
  windowReady: () => invoke<void>('window_ready'),
  setAppearance: (appearance: Omit<Appearance, 'backgroundPath'>) =>
    invoke<ViewState>('set_appearance', { appearance }),
  setBackgroundImage: (path: string) => invoke<ViewState>('set_background_image', { path }),
  removeBackgroundImage: () => invoke<ViewState>('remove_background_image'),
  openDataFolder: () => invoke<void>('open_data_folder'),
  openLogFolder: () => invoke<void>('open_log_folder'),
  listDefaultDevices: () => invoke<DefaultDevicesView>('list_default_devices'),
  setDefaultDevice: (endpointId: string, communications: boolean) =>
    invoke<void>('set_default_device', { endpointId, communications }),
  /** 立即重新检测离线设备；null 表示全部 */
  redetect: (node: number | null) => invoke<void>('redetect', { node }),
  removeNode: (node: number) => invoke<ViewState>('remove_node', { node }),
  renameNode: (node: number, name: string) => invoke<ViewState>('rename_node', { node, name }),
  moveNode: (node: number, x: number, y: number) => invoke<ViewState>('move_node', { node, x, y }),
  changeSlot: (node: number, number: number) => invoke<ViewState>('change_slot', { node, number }),
  routeVerdicts: (node: number, fromOutput: boolean) => invoke<RouteVerdict[]>('route_verdicts', { node, fromOutput }),
  matrixVerdicts: () => invoke<PairVerdict[]>('matrix_verdicts'),
  pinNode: (node: number, pinned: boolean) => invoke<ViewState>('pin_node', { node, pinned }),
  movePin: (node: number, index: number) => invoke<ViewState>('move_pin', { node, index }),
  connect: (from: number, to: number) => invoke<ViewState>('connect', { from, to }),
  disconnect: (from: number, to: number) => invoke<ViewState>('disconnect', { from, to }),
  setRouteEnabled: (from: number, to: number, enabled: boolean) =>
    invoke<ViewState>('set_route_enabled', { from, to, enabled }),
  setRouteGain: (from: number, to: number, gainDb: number) => invoke<ViewState>('set_route_gain', { from, to, gainDb }),
  /** `position`：标签在连线上的位置（0–1） */
  setRouteLabel: (from: number, to: number, position: number) =>
    invoke<ViewState>('set_route_label', { from, to, position }),
  /** 显示或隐藏连线标签 */
  setRouteLabelVisible: (from: number, to: number, visible: boolean) =>
    invoke<ViewState>('set_route_label_visible', { from, to, visible }),
  setNodeGain: (node: number, gainDb: number) => invoke<ViewState>('set_node_gain', { node, gainDb }),
  setNodeMuted: (node: number, muted: boolean) => invoke<ViewState>('set_node_muted', { node, muted }),
  /** `thresholdDb` 为 null 表示关闭门限 */
  setNodeGate: (node: number, thresholdDb: number | null) => invoke<ViewState>('set_node_gate', { node, thresholdDb }),
  undo: () => invoke<ViewState>('undo'),
  setLanguage: (language: LanguagePreference) => invoke<ViewState>('set_language', { language }),
  setLastPage: (page: string) => invoke<ViewState>('set_last_page', { page }),
  listDevices: (direction: 'input' | 'output') => invoke<DeviceChoice[]>('list_devices', { direction }),
  checkSlots: () => invoke<void>('check_slots'),
  setTrayLabels: (labels: { open: string; redetect: string; quit: string }) =>
    invoke<void>('set_tray_labels', { labels }),
}

/** 订阅后端推送的完整状态；返回取消订阅的函数 */
export function onStateChanged(handler: (state: ViewState) => void): Promise<UnlistenFn> {
  return listen<ViewState>(STATE_EVENT, (event) => handler(event.payload))
}

/** 订阅检查更新的进展（检查、下载、待重启） */
export function onUpdateChanged(handler: (update: UpdateView) => void): Promise<UnlistenFn> {
  return listen<UpdateView>(UPDATE_EVENT, (event) => handler(event.payload))
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
