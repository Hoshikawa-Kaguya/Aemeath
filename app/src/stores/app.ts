// 界面状态：后端推来的快照（唯一的状态来源）、当前页面、提示条。
// 所有编辑都经后端执行，返回的新快照整体替换旧的（src-tauri/src/view.rs）。

import { defineStore } from 'pinia'
import { computed, ref, shallowRef } from 'vue'
import { backend, onStateChanged } from '@/api/backend'
import type { CommandError, NodeView, ViewState } from '@/api/types'
import { i18n, localeFor, setLocale } from '@/i18n'

export type Page = 'workbench' | 'devices' | 'apps' | 'tools' | 'settings'

export interface Toast {
  message: string
  /** 提示条带"撤销"按钮 */
  undoable: boolean
}

/** 提示条自动消失的时间 */
const TOAST_MS = 5000

export const useAppStore = defineStore('app', () => {
  const state = shallowRef<ViewState | null>(null)
  const page = ref<Page>('workbench')
  const toast = ref<Toast | null>(null)
  let toastTimer = 0
  /** 已经显示过的最大提示编号 */
  let lastNotice = 0

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
      default:
        return t(`error.${code}`)
    }
  }

  function receive(next: ViewState): void {
    const previous = state.value
    state.value = next
    if (previous === null || JSON.stringify(previous.language) !== JSON.stringify(next.language)) {
      applyLocale(localeFor(next.language))
    }
    showNotices(next, previous === null)
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

  async function start(): Promise<void> {
    await onStateChanged(receive)
    receive(await backend.getState())
  }

  async function undo(): Promise<void> {
    hideToast()
    await run(backend.undo)
  }

  async function removeNode(node: NodeView): Promise<void> {
    if (await run(() => backend.removeNode(node.id))) {
      showToast(i18n.global.t('toast.nodeDeleted', { name: node.name }), true)
    }
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
    disconnect,
    showToast,
    hideToast,
    errorText,
  }
})
