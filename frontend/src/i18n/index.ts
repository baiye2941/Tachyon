import { createI18n } from 'solid-i18n'
import zhCN from './locales/zh-CN'
import enUS from './locales/en-US'

export type Locale = 'zh-CN' | 'en-US'

export const DEFAULT_LOCALE: Locale = 'zh-CN'

export const locales = {
  'zh-CN': zhCN,
  'en-US': enUS,
}

export type MessageKey = keyof typeof zhCN

/**
 * i18n store(solid-i18n createStore 包装)。
 *
 * 组件内用 useI18n() 获取响应式 store(语言切换自动重渲染)。
 * 非组件代码(utils/stores/commands/hooks)用模块级 tr() 获取当前翻译。
 */
export const i18n = createI18n({
  language: DEFAULT_LOCALE,
  locales,
})

export { I18nProvider, useI18n } from 'solid-i18n'

/**
 * 模块级翻译函数(Iteration 14)。
 *
 * 设计动机:useI18n() 是 SolidJS hook,只能在组件作用域调用。但 stores/
 * utils/commands/hooks 等非组件模块也需要翻译错误消息、状态标签等。
 * tr() 提供组件外的翻译入口。
 *
 * 响应式语义:
 * - 在 createMemo / createEffect / JSX 表达式内调用:读取 i18n.t(store
 *   属性)建立追踪,语言切换时随追踪 scope 重算。
 * - 在普通函数(事件回调、catch 块)内调用:返回调用时刻的当前语言字符串,
 *   非响应式(一次性 toast 消息,无需追踪)。
 *
 * @param key locale key(类型化为 MessageKey)
 * @param values 模板插值,如 { name: "foo", count: 3 }。
 *   值类型宽松为 string | number | unknown,以兼容 catch 块的 unknown 错误
 *   (i18n-mini 内部会用 String() 隐式转换)。
 * @returns 翻译后的字符串
 */
export function tr(
  key: MessageKey,
  values?: Record<string, string | number | unknown>,
): string {
  // 读取 i18n.t 属性触发 store 追踪(在 reactive scope 内)
  const fn = i18n.t
  const result = fn(key, values as Record<string, number | string | Date>)
  return Array.isArray(result) ? result.join('') : (result as string)
}
