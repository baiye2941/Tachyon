/**
 * 命令注册表(Iteration 07,DI-1)。
 *
 * 单一数据源(SSOT):所有命令在此定义,CommandPalette 渲染、fuzzy 搜索、
 * 快捷键帮助页都从本模块派生。加新命令只需在此追加一条数据。
 *
 * 设计决策:
 * - 命令是**纯数据 + ctx 解耦的 run**,不捕获渲染层 props。调用方注入
 *   CommandContext,registry 不依赖 SolidJS 组件。
 * - `shortcut` 字段关联快捷键(显示用,实际键绑定在 shortcuts.ts)。
 * - 可单测:命令 id 唯一性、分组完整、run 行为(mock ctx)。
 */

import type { ViewName } from '../types'

export type CommandGroup = 'navigation' | 'action' | 'task'

/** 命令执行所需的能力,由 CommandPalette 注入 */
export interface CommandContext {
  onViewChange: (view: ViewName) => void
  onClose: () => void
  onNewDownload?: () => void
  onPauseAll?: () => void
  onResumeAll?: () => void
}

export interface Command {
  id: string
  label: string
  hint?: string
  group: CommandGroup
  /** Icon 组件名(对应 utils/icons.tsx 的 ICONS 映射) */
  icon: string
  /** 关联快捷键(显示用,键绑定在 shortcuts.ts) */
  shortcut?: string[]
  /** 执行命令,通过 ctx 解耦运行时依赖 */
  run: (ctx: CommandContext) => void
}

/** 命令分组标签(用于命令面板分组渲染) */
export const GROUP_LABELS: Record<CommandGroup, string> = {
  navigation: '导航',
  action: '操作',
  task: '任务',
}

/** 全部命令(单一数据源) */
export const COMMANDS: Command[] = [
  // 导航
  {
    id: 'nav-downloads',
    label: '下载管理',
    hint: '查看所有下载任务',
    group: 'navigation',
    icon: 'list-bullet',
    shortcut: ['Ctrl', '1'],
    run: (c) => {
      c.onViewChange('downloads')
      c.onClose()
    },
  },
  {
    id: 'nav-sniffer',
    label: '资源嗅探',
    hint: '嗅探网页中的可下载资源',
    group: 'navigation',
    icon: 'magnifying-glass',
    shortcut: ['Ctrl', '2'],
    run: (c) => {
      c.onViewChange('sniffer')
      c.onClose()
    },
  },
  {
    id: 'nav-history',
    label: '历史',
    hint: '下载历史记录',
    group: 'navigation',
    icon: 'clock',
    run: (c) => {
      c.onViewChange('history')
      c.onClose()
    },
  },
  {
    id: 'nav-stats',
    label: '统计',
    hint: '下载速度与数据统计',
    group: 'navigation',
    icon: 'chart-bar',
    run: (c) => {
      c.onViewChange('stats')
      c.onClose()
    },
  },
  {
    id: 'nav-settings',
    label: '设置',
    hint: '应用配置与偏好',
    group: 'navigation',
    icon: 'cog-6-tooth',
    shortcut: ['Ctrl', ','],
    run: (c) => {
      c.onViewChange('settings')
      c.onClose()
    },
  },
  // 任务
  {
    id: 'task-new',
    label: '新建下载',
    hint: '添加新的下载任务',
    group: 'task',
    icon: 'plus',
    shortcut: ['Ctrl', 'N'],
    run: (c) => {
      c.onNewDownload?.()
      c.onClose()
    },
  },
  // 操作
  {
    id: 'act-pause-all',
    label: '全部暂停',
    hint: '暂停所有进行中的下载',
    group: 'action',
    icon: 'pause-circle',
    shortcut: ['Ctrl', 'Shift', 'P'],
    run: (c) => {
      c.onPauseAll?.()
      c.onClose()
    },
  },
  {
    id: 'act-resume-all',
    label: '全部恢复',
    hint: '恢复所有已暂停的下载',
    group: 'action',
    icon: 'play',
    shortcut: ['Ctrl', 'Shift', 'R'],
    run: (c) => {
      c.onResumeAll?.()
      c.onClose()
    },
  },
]

/** 按 id 查找命令 */
export function getCommand(id: string): Command | undefined {
  return COMMANDS.find((c) => c.id === id)
}
