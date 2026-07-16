import { describe, it, expect, beforeEach } from 'vitest'
import {
  pushSpeed,
  getHistory,
  getHistoryVersion,
  clearHistory,
  getCurrentSpeed,
} from '../speedHistory'

describe('speedHistory FT-12 响应式', () => {
  beforeEach(() => {
    clearHistory()
  })

  it('pushSpeed 递增 historyVersion', () => {
    const v0 = getHistoryVersion()
    pushSpeed(100)
    expect(getHistoryVersion()).toBe(v0 + 1)
    pushSpeed(200)
    expect(getHistoryVersion()).toBe(v0 + 2)
  })

  it('getHistory 返回推送顺序', () => {
    pushSpeed(1)
    pushSpeed(2)
    pushSpeed(3)
    expect(getHistory()).toEqual([1, 2, 3])
    expect(getCurrentSpeed()).toBe(3)
  })

  it('clearHistory 也递增 version', () => {
    pushSpeed(10)
    const v = getHistoryVersion()
    clearHistory()
    expect(getHistory()).toEqual([])
    expect(getHistoryVersion()).toBe(v + 1)
  })
})
