import { describe, it, expect, beforeEach, vi } from 'vitest'
import { createRoot, createEffect } from 'solid-js'
import type { TaskInfo } from '../../types'

const mockGetTaskList = vi.fn()
const mockReorderTasks = vi.fn()
const mockMoveTask = vi.fn()
const mockAddToast = vi.fn()
const mockAddHistoryRecord = vi.fn()

vi.mock('../../api/invoke', () => ({
  api: {
    getTaskList: (...args: unknown[]) => mockGetTaskList(...args),
    reorderTasks: (...args: unknown[]) => mockReorderTasks(...args),
    moveTask: (...args: unknown[]) => mockMoveTask(...args),
  },
}))

vi.mock('../toast', () => ({
  addToast: (...args: unknown[]) => mockAddToast(...args),
}))

vi.mock('../history', () => ({
  addHistoryRecord: (...args: unknown[]) => mockAddHistoryRecord(...args),
}))

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

describe('downloads store', () => {
  beforeEach(async () => {
    vi.resetModules()
    mockGetTaskList.mockReset()
    mockReorderTasks.mockReset()
    mockMoveTask.mockReset()
    mockAddToast.mockReset()
    mockAddHistoryRecord.mockReset()
    downloadsModule = await import('../downloads')
  })

  it('setTasks 能正确设置任务列表', () => {
    const tasks = [makeTask('t1'), makeTask('t2')]
    downloadsModule.setTasks(tasks)
    expect(downloadsModule.$tasks.get()).toHaveLength(2)
    expect(downloadsModule.$tasks.get()[0]?.id).toBe('t1')
    expect(downloadsModule.$tasks.get()[1]?.id).toBe('t2')
  })

  it('$filteredTasks 根据 currentFilter 正确过滤', () => {
    downloadsModule.setTasks([
      makeTask('t1', { status: 'downloading' }),
      makeTask('t2', { status: 'completed' }),
      makeTask('t3', { status: 'paused' }),
    ])

    downloadsModule.setCurrentFilter('downloading')
    expect(downloadsModule.$filteredTasks.get()).toHaveLength(1)
    expect(downloadsModule.$filteredTasks.get()[0]?.id).toBe('t1')

    downloadsModule.setCurrentFilter('completed')
    expect(downloadsModule.$filteredTasks.get()).toHaveLength(1)
    expect(downloadsModule.$filteredTasks.get()[0]?.id).toBe('t2')

    downloadsModule.setCurrentFilter('incomplete')
    expect(downloadsModule.$filteredTasks.get()).toHaveLength(2)

    downloadsModule.setCurrentFilter('all')
    expect(downloadsModule.$filteredTasks.get()).toHaveLength(3)
  })

  it('$filterCounts 返回正确的计数', () => {
    downloadsModule.setTasks([
      makeTask('t1', { status: 'downloading' }),
      makeTask('t2', { status: 'completed' }),
      makeTask('t3', { status: 'paused' }),
      makeTask('t4', { status: 'connecting' }),
    ])

    const counts = downloadsModule.$filterCounts.get()
    expect(counts.all).toBe(4)
    expect(counts.downloading).toBe(2)
    expect(counts.completed).toBe(1)
    expect(counts.incomplete).toBe(3)
  })

  it('$selectedTask 根据 selectedId 返回正确的任务', () => {
    downloadsModule.setTasks([
      makeTask('t1'),
      makeTask('t2'),
    ])

    downloadsModule.setSelectedId('t2')
    expect(downloadsModule.$selectedTask.get()?.id).toBe('t2')

    downloadsModule.setSelectedId('non-existent')
    expect(downloadsModule.$selectedTask.get()).toBeNull()

    downloadsModule.setSelectedId(null)
    expect(downloadsModule.$selectedTask.get()).toBeNull()
  })

  it('$totalSpeed 计算活跃任务的总速度', () => {
    downloadsModule.setTasks([
      makeTask('t1', { status: 'downloading', speed: 1024 }),
      makeTask('t2', { status: 'completed', speed: 512 }),
      makeTask('t3', { status: 'connecting', speed: 2048 }),
      makeTask('t4', { status: 'paused', speed: 4096 }),
    ])

    expect(downloadsModule.$totalSpeed.get()).toBe(3072)
  })

  it('updateProgress 增量更新只更新收到 progress 的任务，不重建整个数组', () => {
    downloadsModule.setTasks([
      makeTask('t1', { status: 'downloading', speed: 100, downloaded: 100, progress: 0.1 }),
      makeTask('t2', { status: 'downloading', speed: 200, downloaded: 200, progress: 0.2 }),
    ])

    downloadsModule.updateProgress({
      t1: {
        id: 't1',
        progress: 0.5,
        downloaded: 500,
        speed: 150,
        status: 'downloading',
        fragmentsDone: 3,
        fragmentsTotal: 0,
        activeConcurrency: 0,
      },
    })

    expect(downloadsModule.$tasks.get()[0]?.progress).toBe(0.5)
    expect(downloadsModule.$tasks.get()[0]?.speed).toBe(150)
    expect(downloadsModule.$tasks.get()[0]?.downloaded).toBe(500)
    expect(downloadsModule.$tasks.get()[0]?.fragmentsDone).toBe(3)

    expect(downloadsModule.$tasks.get()[1]?.progress).toBe(0.2)
    expect(downloadsModule.$tasks.get()[1]?.speed).toBe(200)
    expect(downloadsModule.$tasks.get()[1]?.downloaded).toBe(200)
    expect(downloadsModule.$tasks.get()[1]?.fragmentsDone).toBe(2)
  })

  it('updateProgress 对未变化任务不触发 reactive 更新', () => {
    downloadsModule.setTasks([
      makeTask('t1', { status: 'downloading', speed: 100, downloaded: 100, progress: 0.1, fragmentsDone: 1 }),
    ])

    let effectRunCount = 0
    const dispose = createRoot((disposeOuter) => {
      createEffect(() => {
        downloadsModule.$totalSpeed.get() // track
        effectRunCount++
      })
      return disposeOuter
    })

    expect(effectRunCount).toBe(1)

    downloadsModule.updateProgress({
      t1: {
        id: 't1',
        progress: 0.1,
        downloaded: 100,
        speed: 100,
        status: 'downloading',
        fragmentsDone: 1,
        fragmentsTotal: 0,
        activeConcurrency: 0,
      },
    })

    expect(effectRunCount).toBe(1)
    dispose()
  })

  it('updateProgress 对变化任务正确更新字段', () => {
    downloadsModule.setTasks([
      makeTask('t1', { status: 'downloading', speed: 100, downloaded: 100, progress: 0.1, fragmentsDone: 1 }),
    ])

    downloadsModule.updateProgress({
      t1: {
        id: 't1',
        progress: 0.75,
        downloaded: 750,
        speed: 250,
        status: 'downloading',
        fragmentsDone: 3,
        fragmentsTotal: 0,
        activeConcurrency: 0,
      },
    })

    const task = downloadsModule.$tasks.get()[0]
    expect(task?.progress).toBe(0.75)
    expect(task?.downloaded).toBe(750)
    expect(task?.speed).toBe(250)
    expect(task?.fragmentsDone).toBe(3)
    expect(task?.status).toBe('downloading')
  })

  it('updateProgress 状态转到 terminal 时写入历史记录', () => {
    downloadsModule.setTasks([
      makeTask('t1', { status: 'downloading', speed: 100, downloaded: 1024, progress: 0.9, fragmentsDone: 3 }),
    ])

    downloadsModule.updateProgress({
      t1: {
        id: 't1',
        progress: 1,
        downloaded: 1024,
        speed: 0,
        status: 'completed',
        fragmentsDone: 4,
        fragmentsTotal: 0,
        activeConcurrency: 0,
      },
    })

    expect(mockAddHistoryRecord).toHaveBeenCalledTimes(1)
    expect(mockAddHistoryRecord).toHaveBeenCalledWith(
      expect.objectContaining({
        status: 'completed',
        fileSize: 1048576,
      }),
    )
  })

  it('updateProgress 对已 terminal 任务重复更新不重复写入历史记录', () => {
    downloadsModule.setTasks([
      makeTask('t1', { status: 'completed', speed: 0, downloaded: 1024, progress: 1, fragmentsDone: 4 }),
    ])

    downloadsModule.updateProgress({
      t1: {
        id: 't1',
        progress: 1,
        downloaded: 1024,
        speed: 0,
        status: 'completed',
        fragmentsDone: 4,
        fragmentsTotal: 0,
        activeConcurrency: 0,
      },
    })

    expect(mockAddHistoryRecord).not.toHaveBeenCalled()
  })

  // ── 审计 FT-04:cold 字段独立变化也必须写入 tasks store ─────────

  it('updateProgress 仅 fragmentsTotal 变化时更新 task', () => {
    downloadsModule.setTasks([
      makeTask('t1', {
        status: 'downloading',
        speed: 100,
        downloaded: 100,
        progress: 0.1,
        fragmentsDone: 0,
        fragmentsTotal: 0,
        activeConcurrency: 0,
      }),
    ])

    downloadsModule.updateProgress({
      t1: {
        id: 't1',
        progress: 0.1,
        downloaded: 100,
        speed: 100,
        status: 'downloading',
        fragmentsDone: 0,
        fragmentsTotal: 16,
        activeConcurrency: 0,
      },
    })

    expect(downloadsModule.$tasks.get()[0]?.fragmentsTotal).toBe(16)
  })

  it('updateProgress 仅 activeConcurrency 变化时更新 task', () => {
    downloadsModule.setTasks([
      makeTask('t1', {
        status: 'downloading',
        speed: 100,
        downloaded: 100,
        progress: 0.1,
        fragmentsDone: 1,
        fragmentsTotal: 8,
        activeConcurrency: 1,
      }),
    ])

    downloadsModule.updateProgress({
      t1: {
        id: 't1',
        progress: 0.1,
        downloaded: 100,
        speed: 100,
        status: 'downloading',
        fragmentsDone: 1,
        fragmentsTotal: 8,
        activeConcurrency: 4,
      },
    })

    expect(downloadsModule.$tasks.get()[0]?.activeConcurrency).toBe(4)
  })

  it('updateProgress 已 failed 时补发 errorReason 写入 task', () => {
    downloadsModule.setTasks([
      makeTask('t1', {
        status: 'failed',
        speed: 0,
        downloaded: 50,
        progress: 0.05,
        fragmentsDone: 0,
        fragmentsTotal: 4,
        errorReason: undefined,
      }),
    ])

    downloadsModule.updateProgress({
      t1: {
        id: 't1',
        progress: 0.05,
        downloaded: 50,
        speed: 0,
        status: 'failed',
        fragmentsDone: 0,
        fragmentsTotal: 4,
        activeConcurrency: 0,
        errorReason: 'HTTP 404',
      },
    })

    expect(downloadsModule.$tasks.get()[0]?.errorReason).toBe('HTTP 404')
  })

  it('updateProgress errorReason 显式 null 清空旧错误', () => {
    downloadsModule.setTasks([
      makeTask('t1', {
        status: 'downloading',
        speed: 100,
        downloaded: 100,
        progress: 0.1,
        fragmentsDone: 1,
        fragmentsTotal: 4,
        errorReason: 'stale',
      }),
    ])

    downloadsModule.updateProgress({
      t1: {
        id: 't1',
        progress: 0.1,
        downloaded: 100,
        speed: 100,
        status: 'downloading',
        fragmentsDone: 1,
        fragmentsTotal: 4,
        activeConcurrency: 0,
        errorReason: null,
      },
    })

    expect(downloadsModule.$tasks.get()[0]?.errorReason).toBeUndefined()
  })

  it('refreshTaskList 成功时更新任务列表', async () => {
    const tasks = [makeTask('t1'), makeTask('t2')]
    mockGetTaskList.mockResolvedValue(tasks)

    await downloadsModule.refreshTaskList()

    expect(downloadsModule.$tasks.get()).toHaveLength(2)
    expect(downloadsModule.$tasks.get()[0]?.id).toBe('t1')
    expect(mockGetTaskList).toHaveBeenCalledTimes(1)
  })

  it('refreshTaskList 失败时调用 addToast', async () => {
    mockGetTaskList.mockRejectedValue(new Error('fetch failed'))

    await downloadsModule.refreshTaskList()

    expect(mockAddToast).toHaveBeenCalledWith(expect.stringContaining('刷新任务列表失败'), 'error')
  })

  it('reorderTasks 乐观更新本地顺序并调用后端', async () => {
    downloadsModule.setTasks([makeTask('t1'), makeTask('t2'), makeTask('t3')])
    mockReorderTasks.mockResolvedValue(undefined)

    await downloadsModule.reorderTasks(['t3', 't1', 't2'])

    expect(downloadsModule.$tasks.get()[0]?.id).toBe('t3')
    expect(downloadsModule.$tasks.get()[1]?.id).toBe('t1')
    expect(downloadsModule.$tasks.get()[2]?.id).toBe('t2')
    expect(mockReorderTasks).toHaveBeenCalledWith(['t3', 't1', 't2'])
  })

  it('reorderTasks 失败时回退到之前顺序', async () => {
    downloadsModule.setTasks([makeTask('t1'), makeTask('t2'), makeTask('t3')])
    mockReorderTasks.mockRejectedValue(new Error('backend failed'))

    await downloadsModule.reorderTasks(['t3', 't1', 't2'])

    expect(downloadsModule.$tasks.get()[0]?.id).toBe('t1')
    expect(downloadsModule.$tasks.get()[1]?.id).toBe('t2')
    expect(downloadsModule.$tasks.get()[2]?.id).toBe('t3')
    expect(mockAddToast).toHaveBeenCalledWith(expect.stringContaining('调整任务顺序失败'), 'error')
  })

  it('moveTask 调用后端并刷新列表', async () => {
    downloadsModule.setTasks([makeTask('t1'), makeTask('t2')])
    mockMoveTask.mockResolvedValue(undefined)
    mockGetTaskList.mockResolvedValue([makeTask('t2'), makeTask('t1')])

    await downloadsModule.moveTask('t1', 't2')

    expect(mockMoveTask).toHaveBeenCalledWith('t1', 't2')
    expect(mockGetTaskList).toHaveBeenCalled()
    expect(downloadsModule.$tasks.get()[0]?.id).toBe('t2')
  })
})
