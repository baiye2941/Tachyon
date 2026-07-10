import { describe, it, expect } from 'vitest'
import {
  toggleSort,
  clearSort,
  sortTasks,
  sortGroupTasks,
  $taskSort,
} from '../taskSort'
import type { TaskInfo } from '../../types'

const task = (overrides: Partial<TaskInfo> = {}): TaskInfo => ({
  id: 'x',
  url: 'http://x',
  fileName: 'file',
  fileSize: 100,
  downloaded: 0,
  speed: 0,
  status: 'pending',
  progress: 0,
  fragmentsTotal: 1,
  fragmentsDone: 0,
  createdAt: '',
  savePath: '',
  ...overrides,
})

describe('toggleSort 状态机', () => {
  it('初始状态:无排序(key=null, dir=desc)', () => {
    clearSort()
    expect($taskSort.state()).toEqual({ key: null, dir: 'desc' })
  })

  it('首次点击某列:设为 desc', () => {
    clearSort()
    toggleSort('speed')
    expect($taskSort.state()).toEqual({ key: 'speed', dir: 'desc' })
  })

  it('同列再次点击:反转方向 desc→asc', () => {
    clearSort()
    toggleSort('speed')
    toggleSort('speed')
    expect($taskSort.state()).toEqual({ key: 'speed', dir: 'asc' })
  })

  it('同列第三次点击:asc→desc', () => {
    clearSort()
    toggleSort('speed')
    toggleSort('speed')
    toggleSort('speed')
    expect($taskSort.state()).toEqual({ key: 'speed', dir: 'desc' })
  })

  it('异列点击:重置为新列 desc', () => {
    clearSort()
    toggleSort('speed')
    toggleSort('speed') // asc
    toggleSort('progress') // 异列,重置 desc
    expect($taskSort.state()).toEqual({ key: 'progress', dir: 'desc' })
  })

  it('clearSort 回到无排序', () => {
    toggleSort('speed')
    clearSort()
    expect($taskSort.state()).toEqual({ key: null, dir: 'desc' })
  })
})

describe('sortTasks 排序', () => {
  const tasks: TaskInfo[] = [
    task({ id: '1', fileName: 'b', progress: 0.5, speed: 100, status: 'downloading' }),
    task({ id: '2', fileName: 'a', progress: 0.9, speed: 50, status: 'completed' }),
    task({ id: '3', fileName: 'c', progress: 0.1, speed: 200, status: 'paused' }),
  ]

  it('key=null 返回原序(浅拷贝)', () => {
    const r = sortTasks(tasks, { key: null, dir: 'desc' })
    expect(r.map((t) => t.id)).toEqual(['1', '2', '3'])
    // 不修改原数组
    expect(r).not.toBe(tasks)
  })

  it('speed desc:速度降序', () => {
    const r = sortTasks(tasks, { key: 'speed', dir: 'desc' })
    expect(r.map((t) => t.speed)).toEqual([200, 100, 50])
  })

  it('speed asc:速度升序', () => {
    const r = sortTasks(tasks, { key: 'speed', dir: 'asc' })
    expect(r.map((t) => t.speed)).toEqual([50, 100, 200])
  })

  it('progress desc:进度降序', () => {
    const r = sortTasks(tasks, { key: 'progress', dir: 'desc' })
    expect(r.map((t) => t.progress)).toEqual([0.9, 0.5, 0.1])
  })

  it('status desc:活跃状态优先(权重低在前)', () => {
    const r = sortTasks(tasks, { key: 'status', dir: 'desc' })
    // downloading(0) < paused(5) < completed(6)
    expect(r.map((t) => t.status)).toEqual(['downloading', 'paused', 'completed'])
  })

  it('同序值时按 fileName 稳定排序', () => {
    const same: TaskInfo[] = [
      task({ id: '1', fileName: 'zebra', speed: 100 }),
      task({ id: '2', fileName: 'apple', speed: 100 }),
    ]
    const r = sortTasks(same, { key: 'speed', dir: 'desc' })
    expect(r.map((t) => t.fileName)).toEqual(['apple', 'zebra'])
  })

  it('不修改原数组', () => {
    const original = [...tasks]
    sortTasks(tasks, { key: 'speed', dir: 'desc' })
    expect(tasks.map((t) => t.id)).toEqual(original.map((t) => t.id))
  })

  it('空列表安全', () => {
    expect(sortTasks([], { key: 'speed', dir: 'desc' })).toEqual([])
  })
})

describe('sortGroupTasks 分组视图组内排序', () => {
  it('使用 progress/speed/status 等全局排序 key 时，与 sortTasks 语义一致', () => {
    const tasks: TaskInfo[] = [
      task({ id: '1', fileName: 'b', progress: 0.5 }),
      task({ id: '2', fileName: 'a', progress: 0.9 }),
      task({ id: '3', fileName: 'c', progress: 0.1 }),
    ]
    const r = sortGroupTasks(tasks, { key: 'progress', dir: 'desc' })
    expect(r.map((t) => t.progress)).toEqual([0.9, 0.5, 0.1])
  })

  it('无排序 key 时按 createdAt 降序 → fileName 升序稳定排序', () => {
    const tasks: TaskInfo[] = [
      task({ id: '1', fileName: 'b', createdAt: '2026-01-01T00:00:00Z' }),
      task({ id: '2', fileName: 'a', createdAt: '2026-01-03T00:00:00Z' }),
      task({ id: '3', fileName: 'c', createdAt: '2026-01-02T00:00:00Z' }),
    ]
    const r = sortGroupTasks(tasks, { key: null, dir: 'desc' })
    expect(r.map((t) => t.id)).toEqual(['2', '3', '1'])
  })

  it('name 列不参与排序，同样回退到 createdAt 降序', () => {
    const tasks: TaskInfo[] = [
      task({ id: '1', fileName: 'a', createdAt: '2026-01-01T00:00:00Z' }),
      task({ id: '2', fileName: 'b', createdAt: '2026-01-02T00:00:00Z' }),
    ]
    const r = sortGroupTasks(tasks, { key: 'name', dir: 'asc' })
    expect(r.map((t) => t.id)).toEqual(['2', '1'])
  })

  it('createdAt 相同则按 fileName 升序稳定', () => {
    const tasks: TaskInfo[] = [
      task({ id: '1', fileName: 'zebra', createdAt: '2026-01-01T00:00:00Z' }),
      task({ id: '2', fileName: 'apple', createdAt: '2026-01-01T00:00:00Z' }),
    ]
    const r = sortGroupTasks(tasks, { key: null, dir: 'desc' })
    expect(r.map((t) => t.fileName)).toEqual(['apple', 'zebra'])
  })

  it('不修改原数组', () => {
    const tasks: TaskInfo[] = [
      task({ id: '1', fileName: 'b', createdAt: '2026-01-01T00:00:00Z' }),
      task({ id: '2', fileName: 'a', createdAt: '2026-01-02T00:00:00Z' }),
    ]
    const original = [...tasks]
    sortGroupTasks(tasks, { key: null, dir: 'desc' })
    expect(tasks).toEqual(original)
  })
})

describe('状态排序权重覆盖所有 DownloadStatus', () => {
  // 确保 STATUS_RANK 无遗漏(类型安全 + 运行时验证)
  it('所有 status 都有权重且可排序', () => {
    const all: TaskInfo[] = (
      ['pending', 'connecting', 'downloading', 'paused', 'resuming', 'verifying', 'completed', 'failed', 'cancelled'] as const
    ).map((s, i) => task({ id: String(i), status: s, fileName: `f${i}` }))
    // desc:权重低在前
    const r = sortTasks(all, { key: 'status', dir: 'desc' })
    expect(r[0]!.status).toBe('downloading')
    expect(r[r.length - 1]!.status).toBe('failed')
  })
})
