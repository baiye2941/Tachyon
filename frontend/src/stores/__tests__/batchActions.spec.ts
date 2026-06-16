import { describe, it, expect, beforeEach, vi } from 'vitest'

const mockPauseTask = vi.fn()
const mockResumeTask = vi.fn()
const mockDeleteTask = vi.fn()
const mockGetTaskList = vi.fn()
const mockConfirm = vi.fn()
const mockAddToast = vi.fn()

vi.mock('../../api/invoke', () => ({
  api: {
    pauseTask: (...args: unknown[]) => mockPauseTask(...args),
    resumeTask: (...args: unknown[]) => mockResumeTask(...args),
    deleteTask: (...args: unknown[]) => mockDeleteTask(...args),
    getTaskList: (...args: unknown[]) => mockGetTaskList(...args),
  },
}))

vi.mock('@tauri-apps/plugin-dialog', () => ({
  confirm: (...args: unknown[]) => mockConfirm(...args),
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
  mockDeleteTask.mockReset()
  mockGetTaskList.mockReset()
  mockConfirm.mockReset()
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
  })

  it('deleteSelected 弹出确认对话框后删除选中任务', async () => {
    downloadsModule.setTasks([makeTask('t1'), makeTask('t2')])
    selectionModule.selectAll(['t1'])
    mockConfirm.mockResolvedValue(true)
    mockDeleteTask.mockResolvedValue(undefined)
    mockGetTaskList.mockResolvedValue([])

    await batchActionsModule.deleteSelected()

    expect(mockConfirm).toHaveBeenCalledWith('确定要删除选中的 1 个任务吗？', expect.any(Object))
    expect(mockDeleteTask).toHaveBeenCalledWith('t1')
    expect(selectionModule.$selectedIds.get().size).toBe(0)
  })

  it('deleteSelected 用户取消时不删除', async () => {
    downloadsModule.setTasks([makeTask('t1')])
    selectionModule.selectAll(['t1'])
    mockConfirm.mockResolvedValue(false)
    mockDeleteTask.mockResolvedValue(undefined)
    mockGetTaskList.mockResolvedValue([])

    await batchActionsModule.deleteSelected()

    expect(mockDeleteTask).not.toHaveBeenCalled()
    expect(selectionModule.$selectedIds.get().size).toBe(1)
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
})
