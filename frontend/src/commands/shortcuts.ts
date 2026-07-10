/**
 * 快捷键数据(Iteration 07,DI-2)。
 *
 * 单一数据源:快捷键帮助页(ShortcutHelp)和命令面板(prompt 提示)
 * 都从本模块派生。加新快捷键只需在此追加一条。
 *
 * 实际键绑定从 stores/shortcuts.ts 读取,支持用户自定义覆盖。
 * Platform 字段在渲染时把 'Ctrl' 替换为 'Cmd'(macOS)。
 */

import type { MessageKey } from '../i18n'

export type ShortcutGroup = 'global' | 'navigation' | 'task' | 'list'

export interface Shortcut {
  /** 键序列(显示用),如 ['Ctrl', 'K'] */
  keys: string[]
  /** i18n key(渲染时翻译) */
  labelKey: MessageKey
  group: ShortcutGroup
  /** 关联的命令 id(命令面板 badge 与全局动作映射用) */
  commandId?: string
}

export const GROUP_LABEL_KEYS: Record<ShortcutGroup, MessageKey> = {
  global: 'shortcutGroup.global',
  navigation: 'shortcutGroup.navigation',
  task: 'shortcutGroup.task',
  list: 'shortcutGroup.list',
}

/** 全部快捷键(单一数据源) */
export const SHORTCUTS: Shortcut[] = [
  // 全局
  { keys: ['Ctrl', 'K'], labelKey: 'shortcut.openCommandPalette', group: 'global' },
  { keys: ['Ctrl', '/'], labelKey: 'shortcut.shortcutHelp', group: 'global' },
  { keys: ['Ctrl', 'B'], labelKey: 'shortcut.toggleSidebar', group: 'global', commandId: 'act-toggle-sidebar' },
  // 导航
  { keys: ['Ctrl', '1'], labelKey: 'shortcut.nav.downloads', group: 'navigation', commandId: 'nav-downloads' },
  { keys: ['Ctrl', '2'], labelKey: 'shortcut.nav.sniffer', group: 'navigation', commandId: 'nav-sniffer' },
  { keys: ['Ctrl', ','], labelKey: 'shortcut.nav.settings', group: 'navigation', commandId: 'nav-settings' },
  // 任务
  { keys: ['Ctrl', 'N'], labelKey: 'shortcut.task.new', group: 'task', commandId: 'task-new' },
  { keys: ['Ctrl', 'Shift', 'P'], labelKey: 'shortcut.task.pauseAll', group: 'task', commandId: 'act-pause-all' },
  { keys: ['Ctrl', 'Shift', 'R'], labelKey: 'shortcut.task.resumeAll', group: 'task', commandId: 'act-resume-all' },
  // 列表
  { keys: ['Enter'], labelKey: 'shortcut.list.openDetail', group: 'list' },
  { keys: ['Space'], labelKey: 'shortcut.list.togglePause', group: 'list' },
  { keys: ['Delete'], labelKey: 'shortcut.list.delete', group: 'list' },
]

/** 把 'Ctrl' 替换为平台对应修饰键(macOS 显示 Cmd) */
export function platformKeys(keys: string[], isMac: boolean): string[] {
  if (!isMac) return keys
  return keys.map((k) => (k === 'Ctrl' ? 'Cmd' : k))
}

/** 按 group 分组 */
export function groupedShortcuts(): Record<ShortcutGroup, Shortcut[]> {
  const result: Record<ShortcutGroup, Shortcut[]> = {
    global: [],
    navigation: [],
    task: [],
    list: [],
  }
  for (const s of SHORTCUTS) {
    result[s.group].push(s)
  }
  return result
}
