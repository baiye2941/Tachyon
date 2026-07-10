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

import type { ViewName, TaskInfo } from '../types'
import type { MessageKey } from '../i18n'

export type CommandGroup = 'navigation' | 'action' | 'task'

/** 命令执行所需的能力,由 CommandPalette 注入 */
export interface CommandContext {
  onViewChange: (view: ViewName) => void
  onClose: () => void
  onNewDownload?: () => void
  onPauseAll?: () => void
  onResumeAll?: () => void
  onCancelAll?: () => void
  onClearCompleted?: () => void
  onToggleSidebar?: () => void
  /** 任务搜索:返回当前任务列表(CommandPalette 任务搜索用) */
  getTasks?: () => { id: string; fileName: string; url: string }[]
  /** 打开指定任务详情(选中任务) */
  onOpenTask?: (taskId: string) => void
  /** 当前选中的任务(用于任务级操作命令) */
  getSelectedTask?: () => TaskInfo | null
  /** 打开指定任务的保存目录 */
  onOpenTaskFolder?: (taskId: string) => void
  /** 重新下载指定任务 */
  onRedownloadTask?: (taskId: string) => void
  /** 复制文本到剪贴板 */
  onCopyToClipboard?: (text: string) => void
}

export interface Command {
  id: string
  /** i18n key(渲染时翻译,避免模块加载时固化语言) */
  labelKey: MessageKey
  /** i18n key(可选) */
  hintKey?: MessageKey
  group: CommandGroup
  /** Icon 组件名(对应 utils/icons.tsx 的 ICONS 映射) */
  icon: string
  /** 关联快捷键(显示用,键绑定在 shortcuts.ts) */
  shortcut?: string[]
  /** 搜索别名(拼音首字母 / 英文等),用于 CommandPalette fuzzy 搜索 */
  aliases?: string[]
  /** 是否在当前上下文中可见;省略则始终显示 */
  visible?: (ctx: CommandContext) => boolean
  /** 执行命令,通过 ctx 解耦运行时依赖 */
  run: (ctx: CommandContext) => void
}

/** 命令分组标签 i18n key */
export const GROUP_LABEL_KEYS: Record<CommandGroup, MessageKey> = {
  navigation: 'commandGroup.navigation',
  action: 'commandGroup.action',
  task: 'commandGroup.task',
}

/** 全部命令(单一数据源) */
export const COMMANDS: Command[] = [
  // 导航
  {
    id: 'nav-downloads',
    labelKey: 'command.nav.downloads.label',
    hintKey: 'command.nav.downloads.hint',
    group: 'navigation',
    icon: 'list-bullet',
    shortcut: ['Ctrl', '1'],
    aliases: ['xz', 'downloads'],
    run: (c) => {
      c.onViewChange('downloads')
      c.onClose()
    },
  },
  {
    id: 'nav-sniffer',
    labelKey: 'command.nav.sniffer.label',
    hintKey: 'command.nav.sniffer.hint',
    group: 'navigation',
    icon: 'magnifying-glass',
    shortcut: ['Ctrl', '2'],
    aliases: ['bf', 'xt', 'sniffer'],
    run: (c) => {
      c.onViewChange('sniffer')
      c.onClose()
    },
  },
  {
    id: 'nav-history',
    labelKey: 'command.nav.history.label',
    hintKey: 'command.nav.history.hint',
    group: 'navigation',
    icon: 'clock',
    aliases: ['ls', 'history'],
    run: (c) => {
      c.onViewChange('history')
      c.onClose()
    },
  },
  {
    id: 'nav-stats',
    labelKey: 'command.nav.stats.label',
    hintKey: 'command.nav.stats.hint',
    group: 'navigation',
    icon: 'chart-bar',
    aliases: ['tj', 'stats'],
    run: (c) => {
      c.onViewChange('stats')
      c.onClose()
    },
  },
  {
    id: 'nav-settings',
    labelKey: 'command.nav.settings.label',
    hintKey: 'command.nav.settings.hint',
    group: 'navigation',
    icon: 'cog-6-tooth',
    shortcut: ['Ctrl', ','],
    aliases: ['sz', 'settings', 'config'],
    run: (c) => {
      c.onViewChange('settings')
      c.onClose()
    },
  },
  // 任务
  {
    id: 'task-new',
    labelKey: 'command.task.new.label',
    hintKey: 'command.task.new.hint',
    group: 'task',
    icon: 'plus',
    shortcut: ['Ctrl', 'N'],
    aliases: ['new', 'xj'],
    run: (c) => {
      c.onNewDownload?.()
      c.onClose()
    },
  },
  // 操作
  {
    id: 'act-pause-all',
    labelKey: 'command.act.pauseAll.label',
    hintKey: 'command.act.pauseAll.hint',
    group: 'action',
    icon: 'pause-circle',
    shortcut: ['Ctrl', 'Shift', 'P'],
    aliases: ['pause', 'zt'],
    run: (c) => {
      c.onPauseAll?.()
      c.onClose()
    },
  },
  {
    id: 'act-resume-all',
    labelKey: 'command.act.resumeAll.label',
    hintKey: 'command.act.resumeAll.hint',
    group: 'action',
    icon: 'play',
    shortcut: ['Ctrl', 'Shift', 'R'],
    aliases: ['resume', 'hf'],
    run: (c) => {
      c.onResumeAll?.()
      c.onClose()
    },
  },
  {
    id: 'act-toggle-sidebar',
    labelKey: 'command.act.toggleSidebar.label',
    hintKey: 'command.act.toggleSidebar.hint',
    group: 'action',
    icon: 'list-bullet',
    shortcut: ['Ctrl', 'B'],
    aliases: ['sidebar', 'ce'],
    run: (c) => {
      c.onToggleSidebar?.()
      c.onClose()
    },
  },
  {
    id: 'act-cancel-all',
    labelKey: 'command.act.cancelAll.label',
    hintKey: 'command.act.cancelAll.hint',
    group: 'action',
    icon: 'cancel',
    aliases: ['cancel', 'qx'],
    run: (c) => {
      c.onCancelAll?.()
      c.onClose()
    },
  },
  {
    id: 'act-clear-completed',
    labelKey: 'command.act.clearCompleted.label',
    hintKey: 'command.act.clearCompleted.hint',
    group: 'action',
    icon: 'trash',
    aliases: ['clear', 'qk'],
    run: (c) => {
      c.onClearCompleted?.()
      c.onClose()
    },
  },
  // 任务级操作(依赖当前选中任务,CommandPalette 通过 visible 控制显示)
  {
    id: 'task-copy-magnet',
    labelKey: 'command.task.copyMagnet.label',
    hintKey: 'command.task.copyMagnet.hint',
    group: 'task',
    icon: 'copy',
    aliases: ['cp', 'copy', 'magnet'],
    visible: (c) => {
      const task = c.getSelectedTask?.()
      return !!task && task.url.startsWith('magnet:')
    },
    run: (c) => {
      const task = c.getSelectedTask?.()
      if (task) {
        c.onCopyToClipboard?.(task.url)
      }
      c.onClose()
    },
  },
  {
    id: 'task-open-folder',
    labelKey: 'command.task.openFolder.label',
    hintKey: 'command.task.openFolder.hint',
    group: 'task',
    icon: 'folder-open',
    aliases: ['open', 'folder'],
    visible: (c) => {
      const task = c.getSelectedTask?.()
      return !!task?.savePath
    },
    run: (c) => {
      const task = c.getSelectedTask?.()
      if (task) {
        c.onOpenTaskFolder?.(task.id)
      }
      c.onClose()
    },
  },
  {
    id: 'task-redownload',
    labelKey: 'command.task.redownload.label',
    hintKey: 'command.task.redownload.hint',
    group: 'task',
    icon: 'refresh',
    aliases: ['re', 'redownload', 'cx'],
    visible: (c) => !!c.getSelectedTask?.(),
    run: (c) => {
      const task = c.getSelectedTask?.()
      if (task) {
        c.onRedownloadTask?.(task.id)
      }
      c.onClose()
    },
  },
]

/** 按 id 查找命令 */
export function getCommand(id: string): Command | undefined {
  return COMMANDS.find((c) => c.id === id)
}
