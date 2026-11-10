// 界面状态：后端推来的快照（唯一的状态来源）、当前页面、提示条。
// 所有编辑都经后端执行，返回的新快照整体替换旧的（src-tauri/src/view.rs）。

import { defineStore } from 'pinia'
import { computed, ref, shallowRef, watch } from 'vue'
import { backend, onStateChanged, onUpdateChanged } from '@/api/backend'
import type { CommandError, HotkeyTargetView, NodeView, UpdateView, ViewState } from '@/api/types'
import { i18n, localeFor, setLocale } from '@/i18n'
import { applyAppearance } from '@/theme/theme'
import { describeTarget } from '@/workbench/hotkeyLabels'

const PAGES = ['workbench', 'devices', 'apps', 'recordings', 'hotkeys', 'tools', 'settings'] as const
export type Page = (typeof PAGES)[number]

export interface Toast {
  message: string
  /** 提示条带"撤销"按钮 */
  undoable: boolean
}

export interface Confirmation {
  message: string
  /** 确认按钮的文字 */
  action: string
  resolve: (accepted: boolean) => void
}

/** 提示条自动消失的时间 */
const TOAST_MS = 5000
/** 切换引擎采样率时，最多等这么久（M4 计划 E3）；超时就解除，界面显示实际格式 */
const RATE_SWITCH_TIMEOUT_MS = 10_000
/** 共享模式下跟随引擎的采样率上限（SLOT_FORMAT_MAX_HZ） */
export const SLOT_FORMAT_MAX_HZ = 384_000

export const useAppStore = defineStore('app', () => {
  const state = shallowRef<ViewState | null>(null)
  const page = ref<Page>('workbench')
  const toast = ref<Toast | null>(null)
  const confirmation = shallowRef<Confirmation | null>(null)
  let toastTimer = 0
  /** 已经显示过的最大提示编号 */
  let lastNotice = 0
  /** 引擎采样率对话框是否打开（标题栏和设置页共用） */
  const rateDialogOpen = ref(false)
  /** 正在切换引擎采样率：等"跟随引擎"的节点读回新格式 */
  const rateSwitching = ref(false)
  let rateSwitchTimer = 0
  const update = shallowRef<UpdateView | null>(null)

  const nodes = computed(() => state.value?.nodes ?? [])
  const routes = computed(() => state.value?.routes ?? [])
  const nodeById = computed(() => new Map(nodes.value.map((node) => [node.id, node])))

  function showToast(message: string, undoable = false): void {
    toast.value = { message, undoable }
    window.clearTimeout(toastTimer)
    toastTimer = window.setTimeout(() => (toast.value = null), TOAST_MS)
  }

  function hideToast(): void {
    window.clearTimeout(toastTimer)
    toast.value = null
  }

  function errorText(error: unknown): string {
    const { t } = i18n.global
    const known = error as Partial<CommandError> | undefined
    const code = known?.code
    switch (code) {
      case 'failed':
        return t('error.failed', { message: (error as { message: string }).message })
      case undefined:
        return t('error.failed', { message: String(error) })
      case 'hotkeyConflict':
        return t('error.hotkeyConflict', {
          object: describeTarget((error as { target: HotkeyTargetView }).target, nodes.value),
        })
      default:
        return t(`error.${code}`)
    }
  }

  function receive(next: ViewState): void {
    const previous = state.value
    state.value = next
    if (previous === null) {
      page.value = PAGES.find((known) => known === next.lastPage) ?? page.value
    }
    if (previous === null || JSON.stringify(previous.language) !== JSON.stringify(next.language)) {
      applyLocale(localeFor(next.language))
    }
    showNotices(next, previous === null)
    if (
      previous === null ||
      previous.appearance.themeMode !== next.appearance.themeMode ||
      previous.appearance.accent !== next.appearance.accent
    ) {
      applyAppearance(next.appearance)
    }
    if (previous === null) {
      // 主题已应用，这时显示窗口不会闪默认色
      backend.windowReady().catch(() => {})
    }
    if (rateSwitching.value && followersAtEngineRate(next)) {
      endRateSwitch()
    }
  }

  /** 在线的"跟随引擎"节点是否都已读回新采样率 */
  function followersAtEngineRate(snapshot: ViewState): boolean {
    const wanted = Math.min(snapshot.engine.sampleRate, SLOT_FORMAT_MAX_HZ)
    return snapshot.nodes.every((node) => {
      if (node.slot === null || node.slotSettings?.format !== null) {
        return true
      }
      const { direction, number } = node.slot
      const row = snapshot.slots.find((slot) => slot.slot.direction === direction && slot.slot.number === number)
      return row?.health.state !== 'healthy' || row.format?.sampleRate === wanted
    })
  }

  function endRateSwitch(): void {
    window.clearTimeout(rateSwitchTimer)
    rateSwitching.value = false
  }

  async function setEngineRate(hz: number): Promise<void> {
    rateSwitching.value = true
    window.clearTimeout(rateSwitchTimer)
    rateSwitchTimer = window.setTimeout(endRateSwitch, RATE_SWITCH_TIMEOUT_MS)
    if (!(await run(() => backend.setEngineRate(hz)))) {
      endRateSwitch()
    }
  }

  /** 后端的一次性提示：只显示没见过的；界面刚打开时，之前的提示都算见过（不重放） */
  function showNotices(next: ViewState, firstSnapshot: boolean): void {
    const fresh = next.notices.filter((notice) => notice.id > lastNotice)
    lastNotice = Math.max(lastNotice, ...next.notices.map((notice) => notice.id))
    const latest = fresh.at(-1)
    if (firstSnapshot || latest === undefined) {
      return
    }
    switch (latest.kind.type) {
      case 'deviceReplaced':
        showToast(i18n.global.t('toast.deviceReplaced', { node: latest.kind.node, device: latest.kind.device }))
        break
      case 'recordingFailed':
        showToast(
          i18n.global.t('toast.recordingFailed', {
            node: latest.kind.node,
            reason: i18n.global.t(`rec.failed.${latest.kind.reason}`),
          }),
        )
        break
      case 'landingRestored':
        showToast(i18n.global.t('toast.landingRestored', { app: latest.kind.app, landing: latest.kind.landing }))
        break
    }
  }

  function applyLocale(locale: string): void {
    setLocale(locale)
    const { t } = i18n.global
    backend.setTrayLabels({ open: t('tray.open'), redetect: t('tray.redetect'), quit: t('tray.quit') }).catch(() => {})
  }

  /** 执行一个返回新快照的命令；失败时显示原因。返回是否成功。 */
  async function run(command: () => Promise<ViewState>): Promise<boolean> {
    try {
      receive(await command())
      return true
    } catch (error) {
      showToast(errorText(error))
      return false
    }
  }

  watch(page, (current) => void run(() => backend.setLastPage(current)))

  async function start(): Promise<void> {
    await onStateChanged(receive)
    await onUpdateChanged((next) => (update.value = next))
    receive(await backend.getState())
    update.value = await backend.getUpdateState()
  }

  async function undo(): Promise<void> {
    hideToast()
    await run(backend.undo)
  }

  /** 打开确认对话框，返回用户是否确认 */
  function confirm(message: string, action: string): Promise<boolean> {
    confirmation.value?.resolve(false)
    return new Promise((resolve) => {
      confirmation.value = { message, action, resolve }
    })
  }

  function answerConfirmation(accepted: boolean): void {
    confirmation.value?.resolve(accepted)
    confirmation.value = null
  }

  async function removeNode(node: NodeView): Promise<void> {
    // 落点节点被删除时提示影响的应用数，但不阻止（M3 计划 L3）。查询失败时按没有应用处理：删除照常可以撤销
    const landed = await backend.landingCount(node.id).catch(() => 0)
    if (landed > 0) {
      const { t } = i18n.global
      if (!(await confirm(t('confirm.deleteLanding', { n: landed }), t('menu.deleteNode')))) {
        return
      }
    }
    showToast(i18n.global.t('toast.nodeDeleting'))
    if (await run(() => backend.removeNode(node.id))) {
      showToast(i18n.global.t('toast.nodeDeleted', { name: node.name }), true)
    }
  }

  function toggleRecording(node: NodeView): void {
    const command = node.recorder?.state.state === 'recording' ? backend.stopRecording : backend.startRecording
    command(node.id).catch((error: unknown) => showToast(errorText(error)))
  }

  async function disconnect(from: number, to: number): Promise<void> {
    const src = nodeById.value.get(from)?.name ?? ''
    const dst = nodeById.value.get(to)?.name ?? ''
    if (await run(() => backend.disconnect(from, to))) {
      showToast(i18n.global.t('toast.routeDeleted', { src, dst }), true)
    }
  }

  return {
    state,
    page,
    toast,
    nodes,
    routes,
    nodeById,
    start,
    run,
    undo,
    removeNode,
    toggleRecording,
    disconnect,
    confirmation,
    confirm,
    answerConfirmation,
    rateDialogOpen,
    rateSwitching,
    setEngineRate,
    update,
    showToast,
    hideToast,
    errorText,
  }
})
