import { describe, it, expect, beforeEach } from 'vitest'
import { pushTaskSpeed, getTaskHistory, clearTaskHistory } from '../taskSpeedHistory'

describe('TaskSpeedHistory 单任务速度历史', () => {
  beforeEach(() => {
    clearTaskHistory('task-a')
    clearTaskHistory('task-b')
    clearTaskHistory('')
  })

  it('初始状态为空数组', () => {
    expect(getTaskHistory('task-a')).toEqual([])
  })

  it('按任务 ID 独立保存速度采样', () => {
    pushTaskSpeed('task-a', 100)
    pushTaskSpeed('task-a', 200)
    pushTaskSpeed('task-b', 50)

    expect(getTaskHistory('task-a')).toEqual([100, 200])
    expect(getTaskHistory('task-b')).toEqual([50])
  })

  it('最多保留最近 60 个采样', () => {
    for (let i = 1; i <= 65; i++) {
      pushTaskSpeed('task-a', i)
    }

    const history = getTaskHistory('task-a')
    expect(history).toHaveLength(60)
    expect(history[0]).toBe(6)
    expect(history[59]).toBe(65)
  })

  it('返回 oldest-to-newest 顺序', () => {
    pushTaskSpeed('task-a', 10)
    pushTaskSpeed('task-a', 20)
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

  it('对空 taskId 做防御,不会抛出异常', () => {
    expect(() => {
      pushTaskSpeed('', 100)
      getTaskHistory('')
      clearTaskHistory('')
    }).not.toThrow()

    expect(getTaskHistory('')).toEqual([])
  })
})
