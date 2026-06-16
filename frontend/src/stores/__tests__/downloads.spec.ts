import { describe, it, expect, beforeEach, vi } from 'vitest'
import { createRoot, createEffect } from 'solid-js'
import type { TaskInfo } from '../../types'

const mockGetTaskList = vi.fn()
const mockAddToast = vi.fn()
const mockAddHistoryRecord = vi.fn()

vi.mock('../../api/invoke', () => ({
  api: {
    getTaskList: (...args: unknown[]) => mockGetTaskList(...args),
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
      },
    })

    expect(mockAddHistoryRecord).not.toHaveBeenCalled()
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
})
