import { createI18n } from 'vue-i18n'
import en from './locales/en.json'

type MessageSchema = typeof en

/** 源语言：所有文案先写英文，其他语言是译文 */
const SOURCE_LOCALE = 'en'

// 按 locales 目录下已有的语言包动态发现可用语言，新增语言不需要改代码
const modules = import.meta.glob<{ default: MessageSchema }>('./locales/*.json', { eager: true })
const messages: Record<string, MessageSchema> = {}
for (const [path, module] of Object.entries(modules)) {
  const locale = /\/([^/]+)\.json$/.exec(path)?.[1]
  if (locale !== undefined) {
    messages[locale] = module.default
  }
}

export const availableLocales: readonly string[] = Object.keys(messages)

/**
 * 按系统语言列表挑选界面语言：先找完全相同的，再找同一种语言的（如 zh-TW → zh-CN），
 * 都没有时用英文。
 */
export function detectLocale(preferred: readonly string[]): string {
  for (const tag of preferred) {
    if (availableLocales.includes(tag)) {
      return tag
    }

    const language = tag.split('-')[0]
    const sameLanguage = availableLocales.find((locale) => locale.split('-')[0] === language)
    if (sameLanguage !== undefined) {
      return sameLanguage
    }
  }

  return SOURCE_LOCALE
}

const initialLocale = detectLocale(navigator.languages)
document.documentElement.lang = initialLocale

export const i18n = createI18n({
  legacy: false,
  locale: initialLocale,
  fallbackLocale: SOURCE_LOCALE,
  messages,
})

/** 偏好设置里的语言（"跟随系统"时按系统语言列表挑选）对应的界面语言 */
export function localeFor(preference: { mode: 'system' } | { mode: 'fixed'; locale: string }): string {
  if (preference.mode === 'fixed' && availableLocales.includes(preference.locale)) {
    return preference.locale
  }
  return detectLocale(navigator.languages)
}

/** 切换界面语言，立即生效（不需要重启） */
export function setLocale(locale: string): void {
  i18n.global.locale.value = locale
  document.documentElement.lang = locale
}
