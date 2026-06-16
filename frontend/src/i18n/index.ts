import { createI18n } from 'solid-i18n'
import zhCN from './locales/zh-CN'
import enUS from './locales/en-US'

export type Locale = 'zh-CN' | 'en-US'

export const DEFAULT_LOCALE: Locale = 'zh-CN'

export const locales = {
  'zh-CN': zhCN,
  'en-US': enUS,
}

export const i18n = createI18n({
  language: DEFAULT_LOCALE,
  locales,
})

export { I18nProvider, useI18n } from 'solid-i18n'
