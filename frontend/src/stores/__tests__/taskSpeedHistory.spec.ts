import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import {
  pushTaskSpeed,
  getTaskHistory,
  clearTaskHistory,
  setTaskSpeedNow,
} from '../taskSpeedHistory'

describe('TaskSpeedHistory 单任务速度历史', () => {
  beforeEach(() => {
    clearTaskHistory('task-a')
    clearTaskHistory('task-b')
    clearTaskHistory('')
    setTaskSpeedNow(null)
  })

  afterEach(() => {
    clearTaskHistory('task-a')
    clearTaskHistory('task-b')
    clearTaskHistory('')
    setTaskSpeedNow(null)
    vi.useRealTimers()
  })

  it('初始状态为空数组', () => {
    expect(getTaskHistory('task-a')).toEqual([])
  })

  it('按任务 ID 独立保存速度采样', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-07-28T00:00:00Z'))

    pushTaskSpeed('task-a', 100)
    vi.advanceTimersByTime(500)
    pushTaskSpeed('task-a', 200)
    pushTaskSpeed('task-b', 50)

    expect(getTaskHistory('task-a')).toEqual([100, 200])
    expect(getTaskHistory('task-b')).toEqual([50])
  })

  it('高频事件按 500ms 时间窗节流并在窗末保留最新速度', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-07-28T00:00:00Z'))

    // t=0 leading 100; t=100/200 窗内只更新 trailing; t=500 刷入 300
    pushTaskSpeed('task-a', 100)
    vi.advanceTimersByTime(100)
    pushTaskSpeed('task-a', 200)
    vi.advanceTimersByTime(100)
    pushTaskSpeed('task-a', 300)

    expect(getTaskHistory('task-a')).toEqual([100])

    vi.advanceTimersByTime(300)
    expect(getTaskHistory('task-a')).toEqual([100, 300])
  })

  it('最多保留最近 60 个采样', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-07-28T00:00:00Z'))

    for (let i = 1; i <= 65; i++) {
      pushTaskSpeed('task-a', i)
      vi.advanceTimersByTime(500)
    }

    const history = getTaskHistory('task-a')
    expect(history).toHaveLength(60)
    expect(history[0]).toBe(6)
    expect(history[59]).toBe(65)
  })

  it('返回 oldest-to-newest 顺序', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-07-28T00:00:00Z'))

    pushTaskSpeed('task-a', 10)
    vi.advanceTimersByTime(500)
    pushTaskSpeed('task-a', 20)
    vi.advanceTimersByTime(500)
    pushTaskSpeed('task-a', 30)

    expect(getTaskHistory('task-a')).toEqual([10, 20, 30])
  })

  it('clearTaskHistory 删除指定任务的历史', () => {
    pushTaskSpeed('task-a', 100)
    pushTaskSpeed('task-b', 200)

    clearTaskHistory('task-a')

    expect(getTaskHistory('task-a')).toEqual([])
    expect(getTaskHistory('task-b')).toEqual([200])
  })

  it('clearTaskHistory 取消未触发的 trailing 定时器', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-07-28T00:00:00Z'))

    pushTaskSpeed('task-a', 100)
    vi.advanceTimersByTime(100)
    pushTaskSpeed('task-a', 200)
    expect(getTaskHistory('task-a')).toEqual([100])

    clearTaskHistory('task-a')
    vi.advanceTimersByTime(500)

    expect(getTaskHistory('task-a')).toEqual([])
  })

  it('对空 taskId 做防御,不会抛出异常', () => {
    expect(() => {
      pushTaskSpeed('', 100)
      getTaskHistory('')
      clearTaskHistory('')
    }).not.toThrow()

    expect(getTaskHistory('')).toEqual([])
  })
})
