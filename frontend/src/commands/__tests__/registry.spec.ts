import { describe, it, expect, vi } from 'vitest'
import { COMMANDS, GROUP_LABEL_KEYS, getCommand, type CommandContext } from '../registry'

const makeCtx = (overrides: Partial<CommandContext> = {}): CommandContext => ({
  onViewChange: vi.fn(),
  onClose: vi.fn(),
  ...overrides,
})

describe('命令注册表(Iteration 07 DI-1)', () => {
  it('所有命令 id 唯一', () => {
    const ids = COMMANDS.map((c) => c.id)
    expect(new Set(ids).size).toBe(ids.length)
  })

  it('每个命令有必需字段(id/labelKey/group/icon/run)', () => {
    for (const c of COMMANDS) {
      expect(c.id).toBeTruthy()
      expect(c.labelKey).toBeTruthy()
      expect(c.group).toBeTruthy()
      expect(c.icon).toBeTruthy()
      expect(typeof c.run).toBe('function')
    }
  })

  it('分组标签覆盖所有出现的 group', () => {
    const groups = new Set(COMMANDS.map((c) => c.group))
    for (const g of groups) {
      expect(GROUP_LABEL_KEYS[g]).toBeTruthy()
    }
  })

  it('包含核心命令(导航 + 任务 + 操作)', () => {
    const ids = COMMANDS.map((c) => c.id)
    expect(ids).toContain('nav-downloads')
    expect(ids).toContain('task-new')
    expect(ids).toContain('act-pause-all')
    expect(ids).toContain('act-resume-all')
    expect(ids).toContain('act-cancel-all')
    expect(ids).toContain('act-clear-completed')
    expect(ids).toContain('task-copy-magnet')
    expect(ids).toContain('task-open-folder')
    expect(ids).toContain('task-redownload')
  })

  it('getCommand 按 id 查找', () => {
    expect(getCommand('nav-downloads')?.labelKey).toBe('command.nav.downloads.label')
    expect(getCommand('nonexistent')).toBeUndefined()
  })

  it('导航命令 run 调用 onViewChange + onClose', () => {
    const ctx = makeCtx()
    const cmd = getCommand('nav-sniffer')!
    cmd.run(ctx)
    expect(ctx.onViewChange).toHaveBeenCalledWith('sniffer')
    expect(ctx.onClose).toHaveBeenCalled()
  })

  it('任务命令 run 调用对应回调', () => {
    const onNewDownload = vi.fn()
    const ctx = makeCtx({ onNewDownload })
    getCommand('task-new')!.run(ctx)
    expect(onNewDownload).toHaveBeenCalled()
    expect(ctx.onClose).toHaveBeenCalled()
  })

  it('操作命令 run 调用 pause/resume/cancel/clear 回调', () => {
    const onPauseAll = vi.fn()
    const onResumeAll = vi.fn()
    const onCancelAll = vi.fn()
    const onClearCompleted = vi.fn()
    getCommand('act-pause-all')!.run(makeCtx({ onPauseAll }))
    getCommand('act-resume-all')!.run(makeCtx({ onResumeAll }))
    getCommand('act-cancel-all')!.run(makeCtx({ onCancelAll }))
    getCommand('act-clear-completed')!.run(makeCtx({ onClearCompleted }))
    expect(onPauseAll).toHaveBeenCalled()
    expect(onResumeAll).toHaveBeenCalled()
    expect(onCancelAll).toHaveBeenCalled()
    expect(onClearCompleted).toHaveBeenCalled()
  })

  it('可选回调缺失时不抛错(防御 undefined)', () => {
    const ctx = makeCtx() // 无 onNewDownload/onPauseAll/onResumeAll
    expect(() => getCommand('task-new')!.run(ctx)).not.toThrow()
    expect(() => getCommand('act-pause-all')!.run(ctx)).not.toThrow()
    expect(() => getCommand('act-cancel-all')!.run(ctx)).not.toThrow()
    expect(() => getCommand('task-redownload')!.run(ctx)).not.toThrow()
  })

  it('任务级命令 visible 依赖选中任务上下文', () => {
    const noTask = makeCtx()
    expect(getCommand('task-copy-magnet')!.visible?.(noTask)).toBe(false)
    expect(getCommand('task-open-folder')!.visible?.(noTask)).toBe(false)
    expect(getCommand('task-redownload')!.visible?.(noTask)).toBe(false)

    const httpTask = makeCtx({ getSelectedTask: () => ({ id: 't1', url: 'https://example.com/f', savePath: '/tmp/f' }) as unknown as import('../../types').TaskInfo })
    expect(getCommand('task-copy-magnet')!.visible?.(httpTask)).toBe(false)
    expect(getCommand('task-open-folder')!.visible?.(httpTask)).toBe(true)
    expect(getCommand('task-redownload')!.visible?.(httpTask)).toBe(true)

    const magnetTask = makeCtx({ getSelectedTask: () => ({ id: 't2', url: 'magnet:?xt=urn:btih:abc', savePath: '' }) as unknown as import('../../types').TaskInfo })
    expect(getCommand('task-copy-magnet')!.visible?.(magnetTask)).toBe(true)
    expect(getCommand('task-open-folder')!.visible?.(magnetTask)).toBe(false)
  })

  it('任务级命令 run 调用对应回调', () => {
    const onCopyToClipboard = vi.fn()
    const onOpenTaskFolder = vi.fn()
    const onRedownloadTask = vi.fn()
    const task = { id: 't1', url: 'magnet:?xt=urn:btih:abc', savePath: '/tmp/f' } as unknown as import('../../types').TaskInfo
    const ctx = makeCtx({ getSelectedTask: () => task, onCopyToClipboard, onOpenTaskFolder, onRedownloadTask })

    getCommand('task-copy-magnet')!.run(ctx)
    expect(onCopyToClipboard).toHaveBeenCalledWith(task.url)

    getCommand('task-open-folder')!.run(ctx)
    expect(onOpenTaskFolder).toHaveBeenCalledWith(task.id)

    getCommand('task-redownload')!.run(ctx)
    expect(onRedownloadTask).toHaveBeenCalledWith(task.id)
  })

  it('有 shortcut 的命令格式正确(字符串数组)', () => {
    for (const c of COMMANDS) {
      if (c.shortcut) {
        expect(Array.isArray(c.shortcut)).toBe(true)
        expect(c.shortcut.length).toBeGreaterThan(0)
      }
    }
  })
})
