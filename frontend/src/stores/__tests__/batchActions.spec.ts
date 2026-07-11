import { describe, it, expect, beforeEach, vi } from 'vitest'

const mockPauseTask = vi.fn()
const mockResumeTask = vi.fn()
const mockCancelTask = vi.fn()
const mockDeleteTask = vi.fn()
const mockGetTaskList = vi.fn()
const mockOpenFolder = vi.fn()
const mockCreateTask = vi.fn()
const mockRequestConfirm = vi.fn()
const mockAddToast = vi.fn()

// Iteration 11:不再 mock 整个 api 模块(会掩盖 invoke 包装层副作用),
// 改为 mock confirm store + 真实 api(其 deleteTask 接收 opts.skipConfirm)。
vi.mock('../../api/invoke', () => ({
  api: {
    pauseTask: (...args: unknown[]) => mockPauseTask(...args),
    resumeTask: (...args: unknown[]) => mockResumeTask(...args),
    cancelTask: (...args: unknown[]) => mockCancelTask(...args),
    deleteTask: (...args: unknown[]) => mockDeleteTask(...args),
    getTaskList: (...args: unknown[]) => mockGetTaskList(...args),
    openFolder: (...args: unknown[]) => mockOpenFolder(...args),
    createTask: (...args: unknown[]) => mockCreateTask(...args),
  },
}))

vi.mock('../confirm', () => ({
  requestConfirm: (...args: unknown[]) => mockRequestConfirm(...args),
}))

vi.mock('../toast', () => ({
  addToast: (...args: unknown[]) => mockAddToast(...args),
}))

import type { TaskInfo } from '../../types'

const makeTask = (id: string, overrides: Partial<TaskInfo> = {}): TaskInfo => ({
  id,
  url: `https://example.com/${id}.bin`,
  fileName: `${id}.bin`,
  fileSize: 1048576,
  downloaded: 0,
  speed: 0,
  status: 'downloading',
  progress: 0.5,
  fragmentsTotal: 4,
  fragmentsDone: 2,
  createdAt: '2026-05-30T00:00:00Z',
  savePath: '/downloads',
  ...overrides,
})

let downloadsModule: typeof import('../downloads')
let selectionModule: typeof import('../selection')
let batchActionsModule: typeof import('../batchActions')

beforeEach(async () => {
  vi.resetModules()
  mockPauseTask.mockReset()
  mockResumeTask.mockReset()
  mockCancelTask.mockReset()
  mockDeleteTask.mockReset()
  mockGetTaskList.mockReset()
  mockOpenFolder.mockReset()
  mockCreateTask.mockReset()
  mockRequestConfirm.mockReset()
  mockAddToast.mockReset()

  downloadsModule = await import('../downloads')
  selectionModule = await import('../selection')
  batchActionsModule = await import('../batchActions')
  selectionModule.deselectAll()
})

describe('batchActions store', () => {
  it('pauseSelected 暂停选中任务并清空选择', async () => {
    downloadsModule.setTasks([makeTask('t1'), makeTask('t2'), makeTask('t3')])
    selectionModule.selectAll(['t1', 't2'])
    mockPauseTask.mockResolvedValue(undefined)
    mockGetTaskList.mockResolvedValue([])

    await batchActionsModule.pauseSelected()

    expect(mockPauseTask).toHaveBeenCalledWith('t1')
    expect(mockPauseTask).toHaveBeenCalledWith('t2')
    expect(mockPauseTask).not.toHaveBeenCalledWith('t3')
    expect(selectionModule.$selectedIds.get().size).toBe(0)
    expect(mockAddToast).toHaveBeenCalledWith('已暂停 2 个任务', 'success')
  })

  it('resumeSelected 恢复选中任务', async () => {
    downloadsModule.setTasks([
      makeTask('t1', { status: 'paused' }),
      makeTask('t2', { status: 'paused' }),
    ])
    selectionModule.selectAll(['t1', 't2'])
    mockResumeTask.mockResolvedValue(undefined)
    mockGetTaskList.mockResolvedValue([])

    await batchActionsModule.resumeSelected()

    expect(mockResumeTask).toHaveBeenCalledWith('t1')
    expect(mockResumeTask).toHaveBeenCalledWith('t2')
    expect(mockAddToast).toHaveBeenCalledWith('已恢复 2 个任务', 'success')
  })

  it('deleteSelected 确认后删除并透传 skipConfirm:true', async () => {
    downloadsModule.setTasks([makeTask('t1'), makeTask('t2')])
    selectionModule.selectAll(['t1'])
    mockRequestConfirm.mockResolvedValue({ ok: true, deleteLocalFile: false })
    mockDeleteTask.mockResolvedValue(undefined)
    mockGetTaskList.mockResolvedValue([])

    await batchActionsModule.deleteSelected()

    // Iteration 11:走应用层 ConfirmDialog,不再依赖 Tauri plugin-dialog
    expect(mockRequestConfirm).toHaveBeenCalledTimes(1)
    expect(mockRequestConfirm).toHaveBeenCalledWith(
      expect.objectContaining({
        title: '删除选中任务',
        tone: 'danger',
      }),
    )
    // 关键断言:deleteTask 收到 skipConfirm:true,跳过 invoke 内 window.confirm
    expect(mockDeleteTask).toHaveBeenCalledWith('t1', { skipConfirm: true, deleteLocalFile: false })
    expect(selectionModule.$selectedIds.get().size).toBe(0)
  })

  it('deleteSelected 用户取消时不删除', async () => {
    downloadsModule.setTasks([makeTask('t1')])
    selectionModule.selectAll(['t1'])
    mockRequestConfirm.mockResolvedValue({ ok: false, deleteLocalFile: false })
    mockDeleteTask.mockResolvedValue(undefined)
    mockGetTaskList.mockResolvedValue([])

    await batchActionsModule.deleteSelected()

    expect(mockDeleteTask).not.toHaveBeenCalled()
    expect(selectionModule.$selectedIds.get().size).toBe(1)
  })

  it('deleteSelected 批量 10 任务只弹一次确认', async () => {
    // Iteration 11 回归测试:防止级联 confirm 灾难复发
    const ids = Array.from({ length: 10 }, (_, i) => `t${i}`)
    downloadsModule.setTasks(ids.map(id => makeTask(id)))
    selectionModule.selectAll(ids)
    mockRequestConfirm.mockResolvedValue({ ok: true, deleteLocalFile: false })
    mockDeleteTask.mockResolvedValue(undefined)
    mockGetTaskList.mockResolvedValue([])

    await batchActionsModule.deleteSelected()

    // 确认请求只应有一次(而非 N+1)
    expect(mockRequestConfirm).toHaveBeenCalledTimes(1)
    // 每个 deleteTask 都传 skipConfirm:true
    expect(mockDeleteTask).toHaveBeenCalledTimes(10)
    ids.forEach(id => {
      expect(mockDeleteTask).toHaveBeenCalledWith(id, { skipConfirm: true, deleteLocalFile: false })
    })
    expect(mockAddToast).toHaveBeenCalledWith('已删除 10 个任务记录', 'success')
  })

  it('pauseSelected 部分失败时显示成功与失败汇总', async () => {
    downloadsModule.setTasks([makeTask('t1'), makeTask('t2'), makeTask('t3')])
    selectionModule.selectAll(['t1', 't2', 't3'])
    mockPauseTask
      .mockResolvedValueOnce(undefined)
      .mockRejectedValueOnce(new Error('busy'))
      .mockResolvedValueOnce(undefined)
    mockGetTaskList.mockResolvedValue([])

    await batchActionsModule.pauseSelected()

    expect(mockAddToast).toHaveBeenCalledWith('已暂停 2 个任务', 'success')
    expect(mockAddToast).toHaveBeenCalledWith(
      expect.stringContaining('1 个任务暂停失败'),
      'error',
    )
  })

  it('resumeSelected 部分失败时显示成功与失败汇总', async () => {
    downloadsModule.setTasks([
      makeTask('t1', { status: 'paused' }),
      makeTask('t2', { status: 'paused' }),
    ])
    selectionModule.selectAll(['t1', 't2'])
    mockResumeTask.mockResolvedValueOnce(undefined).mockRejectedValueOnce(new Error('gone'))
    mockGetTaskList.mockResolvedValue([])

    await batchActionsModule.resumeSelected()

    expect(mockAddToast).toHaveBeenCalledWith('已恢复 1 个任务', 'success')
    expect(mockAddToast).toHaveBeenCalledWith(
      expect.stringContaining('1 个任务恢复失败'),
      'error',
    )
  })

  it('pauseAll 暂停所有活跃任务', async () => {
    downloadsModule.setTasks([
      makeTask('t1', { status: 'downloading' }),
      makeTask('t2', { status: 'connecting' }),
      makeTask('t3', { status: 'paused' }),
      makeTask('t4', { status: 'completed' }),
    ])
    mockPauseTask.mockResolvedValue(undefined)
    mockGetTaskList.mockResolvedValue([])

    await batchActionsModule.pauseAll()

    expect(mockPauseTask).toHaveBeenCalledWith('t1')
    expect(mockPauseTask).toHaveBeenCalledWith('t2')
    expect(mockPauseTask).not.toHaveBeenCalledWith('t3')
    expect(mockPauseTask).not.toHaveBeenCalledWith('t4')
  })

  it('pauseAll 没有可暂停任务时提示', async () => {
    downloadsModule.setTasks([
      makeTask('t1', { status: 'paused' }),
      makeTask('t2', { status: 'completed' }),
    ])

    await batchActionsModule.pauseAll()

    expect(mockPauseTask).not.toHaveBeenCalled()
    expect(mockAddToast).toHaveBeenCalledWith('没有可暂停的任务', 'info')
  })

  it('resumeAll 恢复所有已暂停任务', async () => {
    downloadsModule.setTasks([
      makeTask('t1', { status: 'paused' }),
      makeTask('t2', { status: 'paused' }),
      makeTask('t3', { status: 'downloading' }),
    ])
    mockResumeTask.mockResolvedValue(undefined)
    mockGetTaskList.mockResolvedValue([])

    await batchActionsModule.resumeAll()

    expect(mockResumeTask).toHaveBeenCalledWith('t1')
    expect(mockResumeTask).toHaveBeenCalledWith('t2')
    expect(mockResumeTask).not.toHaveBeenCalledWith('t3')
  })

  it('resumeAll 没有可恢复任务时提示', async () => {
    downloadsModule.setTasks([
      makeTask('t1', { status: 'downloading' }),
      makeTask('t2', { status: 'completed' }),
    ])

    await batchActionsModule.resumeAll()

    expect(mockResumeTask).not.toHaveBeenCalled()
    expect(mockAddToast).toHaveBeenCalledWith('没有可恢复的任务', 'info')
  })

  it('cancelSelected 确认后取消选中任务(中性 tone,保留记录)', async () => {
    downloadsModule.setTasks([makeTask('t1'), makeTask('t2')])
    selectionModule.selectAll(['t1'])
    mockRequestConfirm.mockResolvedValue({ ok: true, deleteLocalFile: false })
    mockCancelTask.mockResolvedValue(undefined)
    mockGetTaskList.mockResolvedValue([])

    await batchActionsModule.cancelSelected()

    expect(mockRequestConfirm).toHaveBeenCalledTimes(1)
    expect(mockRequestConfirm).toHaveBeenCalledWith(
      expect.objectContaining({ title: '取消选中任务' }),
    )
    expect(mockCancelTask).toHaveBeenCalledWith('t1')
    expect(selectionModule.$selectedIds.get().size).toBe(0)
    expect(mockAddToast).toHaveBeenCalledWith('已取消 1 个任务', 'success')
  })

  it('cancelSelected 用户取消时不执行', async () => {
    downloadsModule.setTasks([makeTask('t1')])
    selectionModule.selectAll(['t1'])
    mockRequestConfirm.mockResolvedValue({ ok: false, deleteLocalFile: false })

    await batchActionsModule.cancelSelected()

    expect(mockCancelTask).not.toHaveBeenCalled()
    expect(selectionModule.$selectedIds.get().size).toBe(1)
  })

  it('cancelAll 取消所有活跃与暂停任务', async () => {
    downloadsModule.setTasks([
      makeTask('t1', { status: 'downloading' }),
      makeTask('t2', { status: 'paused' }),
      makeTask('t3', { status: 'resuming' }),
      makeTask('t4', { status: 'completed' }),
    ])
    mockRequestConfirm.mockResolvedValue({ ok: true, deleteLocalFile: false })
    mockCancelTask.mockResolvedValue(undefined)
    mockGetTaskList.mockResolvedValue([])

    await batchActionsModule.cancelAll()

    expect(mockCancelTask).toHaveBeenCalledWith('t1')
    expect(mockCancelTask).toHaveBeenCalledWith('t2')
    expect(mockCancelTask).toHaveBeenCalledWith('t3')
    expect(mockCancelTask).not.toHaveBeenCalledWith('t4')
  })

  it('cancelAll 没有可取消任务时提示', async () => {
    downloadsModule.setTasks([
      makeTask('t1', { status: 'completed' }),
      makeTask('t2', { status: 'failed' }),
    ])

    await batchActionsModule.cancelAll()

    expect(mockCancelTask).not.toHaveBeenCalled()
    expect(mockAddToast).toHaveBeenCalledWith('没有可取消的任务', 'info')
  })

  it('openSelectedFolders 打开选中任务目录并提示结果', async () => {
    downloadsModule.setTasks([
      makeTask('t1', { savePath: '/downloads/a/' }),
      makeTask('t2', { savePath: '/downloads/b/' }),
      makeTask('t3', { savePath: '' }),
    ])
    selectionModule.selectAll(['t1', 't2', 't3'])
    mockOpenFolder.mockResolvedValue(undefined)

    await batchActionsModule.openSelectedFolders()

    expect(mockOpenFolder).toHaveBeenCalledWith('/downloads/a')
    expect(mockOpenFolder).toHaveBeenCalledWith('/downloads/b')
    expect(mockOpenFolder).not.toHaveBeenCalledWith('')
    expect(mockAddToast).toHaveBeenCalledWith('已打开 2 个文件夹', 'success')
    expect(mockAddToast).toHaveBeenCalledWith('1 个任务暂无保存路径', 'info')
  })

  it('copySelectedLinks 将选中任务 URL 复制到剪贴板', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined)
    vi.stubGlobal('navigator', {
      ...navigator,
      clipboard: { writeText },
    })

    downloadsModule.setTasks([
      makeTask('t1', { url: 'https://example.com/a.bin' }),
      makeTask('t2', { url: 'https://example.com/b.bin' }),
    ])
    selectionModule.selectAll(['t1', 't2'])

    await batchActionsModule.copySelectedLinks()

    expect(writeText).toHaveBeenCalledWith('https://example.com/a.bin\nhttps://example.com/b.bin')
    expect(mockAddToast).toHaveBeenCalledWith('已复制 2 个链接', 'success')
    vi.unstubAllGlobals()
  })

  it('redownloadSelected 为选中任务创建新任务并清空选择', async () => {
    downloadsModule.setTasks([
      makeTask('t1', { url: 'https://example.com/a.bin' }),
      makeTask('t2', { url: 'https://example.com/b.bin' }),
    ])
    selectionModule.selectAll(['t1', 't2'])
    mockCreateTask.mockResolvedValue(undefined)
    mockGetTaskList.mockResolvedValue([])

    await batchActionsModule.redownloadSelected()

    expect(mockCreateTask).toHaveBeenCalledWith('https://example.com/a.bin')
    expect(mockCreateTask).toHaveBeenCalledWith('https://example.com/b.bin')
    expect(selectionModule.$selectedIds.get().size).toBe(0)
    expect(mockAddToast).toHaveBeenCalledWith('已重新下载 2 个任务', 'success')
  })

  it('redownloadSelected 部分失败时显示成功与失败汇总', async () => {
    downloadsModule.setTasks([
      makeTask('t1', { url: 'https://example.com/a.bin' }),
      makeTask('t2', { url: 'https://example.com/b.bin' }),
    ])
    selectionModule.selectAll(['t1', 't2'])
    mockCreateTask
      .mockResolvedValueOnce(undefined)
      .mockRejectedValueOnce(new Error('invalid'))
    mockGetTaskList.mockResolvedValue([])

    await batchActionsModule.redownloadSelected()

    expect(mockAddToast).toHaveBeenCalledWith('已重新下载 1 个任务', 'success')
    expect(mockAddToast).toHaveBeenCalledWith(
      expect.stringContaining('1 个任务重新下载失败'),
      'error',
    )
  })
})
