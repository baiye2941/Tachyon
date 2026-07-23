import { createSignal } from 'solid-js'
import type { MessageKey } from '../i18n'
import { SHORTCUTS } from '../commands/shortcuts'

const STORAGE_KEY = 'tachyon.shortcuts'

interface ShortcutOverrides {
  version: number
  overrides: Partial<Record<MessageKey, string[]>>
}

const MODIFIERS = new Set(['Ctrl', 'Alt', 'Shift', 'Meta'])
const MODIFIER_ORDER = ['Ctrl', 'Alt', 'Shift', 'Meta']

/** 默认键位映射：从 SHORTCUTS 派生的单一数据源 */
export const DEFAULT_BINDINGS: Record<MessageKey, string[]> = Object.fromEntries(
  SHORTCUTS.map((s) => [s.labelKey, [...s.keys]]),
) as Record<MessageKey, string[]>

const [overrides, setOverrides] = createSignal<Partial<Record<MessageKey, string[]>>>({})

/**
 * 平台检测:优先 NavigatorUAData.platform,回退 userAgent。
 * 不再使用已弃用的 navigator.platform(E-08)。
 */
export function isMacPlatform(): boolean {
  if (typeof navigator === 'undefined') return false
  const uaData = (navigator as Navigator & { userAgentData?: { platform?: string } }).userAgentData
  if (uaData?.platform) {
    return /mac|iphone|ipad|ipod/i.test(uaData.platform)
  }
  return /Mac|iPhone|iPad|iPod/.test(navigator.userAgent)
}

function arraysEqual<T>(a: T[], b: T[]): boolean {
  if (a.length !== b.length) return false
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false
  }
  return true
}

function normalizeKey(key: string): string {
  const trimmed = key.trim()
  if (trimmed === '' || trimmed === ' ') return 'Space'
  if (trimmed.length === 1 && /[a-zA-Z]/.test(trimmed)) return trimmed.toUpperCase()
  return trimmed
}

function normalizeKeys(keys: string[]): string[] {
  const modifierSet = new Set<string>()
  const mainKeys: string[] = []
  for (const key of keys) {
    const normalized = normalizeKey(key)
    if (MODIFIERS.has(normalized)) {
      modifierSet.add(normalized)
    } else {
      mainKeys.push(normalized)
    }
  }
  const modifiers = MODIFIER_ORDER.filter((m) => modifierSet.has(m))
  return [...modifiers, ...mainKeys]
}

function persist(data: Partial<Record<MessageKey, string[]>>): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ version: 1, overrides: data }))
  } catch {
    // 忽略存储失败（如隐私模式）
  }
}

/** 启动时从 localStorage 加载覆盖配置 */
export function loadShortcuts(): void {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return
    const parsed = JSON.parse(raw) as ShortcutOverrides
    if (parsed.version !== 1 || !parsed.overrides || typeof parsed.overrides !== 'object') return

    const validOverrides: Partial<Record<MessageKey, string[]>> = {}
    for (const [key, value] of Object.entries(parsed.overrides)) {
      if (Array.isArray(value) && value.every((v) => typeof v === 'string')) {
        validOverrides[key as MessageKey] = normalizeKeys(value)
      }
    }
    setOverrides(validOverrides)
  } catch {
    // 忽略损坏的存储数据
  }
}

/** 返回当前生效键位（覆盖 → 默认） */
export function getShortcutKeys(labelKey: MessageKey): string[] {
  const override = overrides()[labelKey]
  if (override && override.length > 0) return override
  return DEFAULT_BINDINGS[labelKey] ?? []
}

/** 写入覆盖并持久化 */
export function setShortcut(labelKey: MessageKey, keys: string[]): void {
  const normalized = normalizeKeys(keys)
  const next = { ...overrides(), [labelKey]: normalized }
  setOverrides(next)
  persist(next)
}

/** 移除单条覆盖 */
export function resetShortcut(labelKey: MessageKey): void {
  const next = { ...overrides() }
  delete next[labelKey]
  setOverrides(next)
  persist(next)
}

/** 清空所有覆盖 */
export function resetAllShortcuts(): void {
  setOverrides({})
  persist({})
}

function hasModifier(event: KeyboardEvent, modifier: string, isMac: boolean): boolean {
  switch (modifier) {
    case 'Ctrl':
      return event.ctrlKey || (isMac && event.metaKey)
    case 'Alt':
      return event.altKey
    case 'Shift':
      return event.shiftKey
    case 'Meta':
      return event.metaKey
    default:
      return false
  }
}

/**
 * 判断键盘事件是否命中某条绑定。
 *
 * 规则：
 * - 修饰符顺序固定 Ctrl/Alt/Shift/Meta
 * - 主键大小写不敏感
 * - macOS 下 metaKey 与 ctrlKey 均可命中 Ctrl 标记
 * - 额外的未预期修饰键导致不匹配
 */
export function matchKeyboardEvent(event: KeyboardEvent, labelKey: MessageKey): boolean {
  const keys = getShortcutKeys(labelKey)
  if (keys.length === 0) return false

  const isMac = isMacPlatform()
  const expectedModifiers = keys.filter((k) => MODIFIERS.has(k))
  const mainKeys = keys.filter((k) => !MODIFIERS.has(k))
  if (mainKeys.length !== 1) return false
  const mainKey = mainKeys[0]!

  for (const mod of expectedModifiers) {
    if (!hasModifier(event, mod, isMac)) return false
  }

  const activeModifiers = new Set<string>()
  if (event.ctrlKey || (isMac && event.metaKey)) activeModifiers.add('Ctrl')
  if (event.altKey) activeModifiers.add('Alt')
  if (event.shiftKey) activeModifiers.add('Shift')
  if (event.metaKey && !isMac) activeModifiers.add('Meta')

  if (activeModifiers.size !== expectedModifiers.length) return false
  for (const mod of expectedModifiers) {
    if (!activeModifiers.has(mod)) return false
  }

  const eventKey = event.key === ' ' ? 'Space' : event.key
  return eventKey.toLowerCase() === mainKey.toLowerCase()
}

/**
 * 检测 keys 是否与其他已生效快捷键冲突。
 *
 * 与自身当前绑定相同不算冲突；返回冲突项的 labelKey。
 */
export function findConflict(labelKey: MessageKey, keys: string[]): MessageKey | undefined {
  const normalized = normalizeKeys(keys).map((k) => k.toLowerCase())
  const own = getShortcutKeys(labelKey).map((k) => k.toLowerCase())
  if (arraysEqual(normalized, own)) return undefined

  for (const s of SHORTCUTS) {
    if (s.labelKey === labelKey) continue
    const other = getShortcutKeys(s.labelKey).map((k) => k.toLowerCase())
    if (arraysEqual(normalized, other)) return s.labelKey
  }
  return undefined
}

const COMMAND_ID_TO_LABEL_KEY: Record<string, MessageKey> = {}
for (const s of SHORTCUTS) {
  if (s.commandId) {
    COMMAND_ID_TO_LABEL_KEY[s.commandId] = s.labelKey
  }
}

/** 根据命令 id 找到对应快捷键键位；无映射返回 undefined */
export function getCommandShortcutKeys(commandId: string | undefined): string[] | undefined {
  if (!commandId) return undefined
  const labelKey = COMMAND_ID_TO_LABEL_KEY[commandId]
  if (!labelKey) return undefined
  return getShortcutKeys(labelKey)
}
