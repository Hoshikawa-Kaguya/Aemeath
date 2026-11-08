// 选择可执行文件（应用源）。添加节点和节点设置共用。

import { open } from '@tauri-apps/plugin-dialog'
import { i18n } from '@/i18n'

export async function pickProgram(): Promise<string | null> {
  return open({
    multiple: false,
    directory: false,
    filters: [{ name: i18n.global.t('add.programFilter'), extensions: ['exe'] }],
  })
}
