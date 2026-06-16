/**
 * 快捷键数据(Iteration 07,DI-2)。
 *
 * 单一数据源:快捷键帮助页(ShortcutHelp)和命令面板(prompt 提示)
 * 都从本模块派生。加新快捷键只需在此追加一条。
 *
 * 实际键绑定在 App.tsx 的 handleGlobalKey 中实现,本表用于显示与文档化。
 * Platform 字段在渲染时把 'Ctrl' 替换为 'Cmd'(macOS)。
 */

export type ShortcutGroup = 'global' | 'navigation' | 'task' | 'list'

export interface Shortcut {
  /** 键序列(显示用),如 ['Ctrl', 'K'] */
  keys: string[]
  /** 描述 */
  label: string
  group: ShortcutGroup
}

export const GROUP_LABELS: Record<ShortcutGroup, string> = {
  global: '全局',
  navigation: '导航',
  task: '任务',
  list: '列表',
}

/** 全部快捷键(单一数据源) */
export const SHORTCUTS: Shortcut[] = [
  // 全局
  { keys: ['Ctrl', 'K'], label: '打开命令面板', group: 'global' },
  { keys: ['Ctrl', '/'], label: '快捷键帮助(本页)', group: 'global' },
  // 导航
  { keys: ['Ctrl', '1'], label: '下载管理', group: 'navigation' },
  { keys: ['Ctrl', '2'], label: '资源嗅探', group: 'navigation' },
  { keys: ['Ctrl', ','], label: '设置', group: 'navigation' },
  // 任务
  { keys: ['Ctrl', 'N'], label: '新建下载', group: 'task' },
  { keys: ['Ctrl', 'Shift', 'P'], label: '全部暂停', group: 'task' },
  { keys: ['Ctrl', 'Shift', 'R'], label: '全部恢复', group: 'task' },
  // 列表
  { keys: ['Enter'], label: '打开任务详情', group: 'list' },
  { keys: ['Space'], label: '暂停/恢复选中任务', group: 'list' },
  { keys: ['Delete'], label: '删除选中任务', group: 'list' },
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
