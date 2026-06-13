import { describe, it, expect, beforeEach, vi } from 'vitest'
import type { HistoryRecord, HistoryStats, HistoryFilter } from '../history'

const STORAGE_KEY = 'tachyon:download_history'

interface HistoryStore {
  records: HistoryRecord[]
  addRecord: (record: Omit<HistoryRecord, 'id' | 'completedAt'>) => void
  getRecords: (filter?: HistoryFilter) => HistoryRecord[]
  getStats: () => HistoryStats
  clearHistory: () => void
  getRecordById: (id: string) => HistoryRecord | undefined
}

let historyStore: HistoryStore

beforeEach(async () => {
  localStorage.clear()
  vi.resetModules()
  const mod = await import('../history')
  historyStore = {
    records: mod.historyRecords,
    addRecord: mod.addHistoryRecord,
    getRecords: mod.getHistoryRecords,
    getStats: mod.getHistoryStats,
    clearHistory: mod.clearHistory,
    getRecordById: mod.getRecordById,
  }
})

describe('HistoryStore 历史记录存储', () => {
  it('初始状态为空数组', () => {
    expect(historyStore.records).toEqual([])
  })

  it('添加记录后包含该记录', () => {
    historyStore.addRecord({
      url: 'https://example.com/file.zip',
      fileName: 'file.zip',
      fileSize: 1024 * 1024,
      status: 'completed',
      duration: 5000,
      avgSpeed: 204800,
    })

    expect(historyStore.records).toHaveLength(1)
    expect(historyStore.records[0]?.fileName).toBe('file.zip')
    expect(historyStore.records[0]?.status).toBe('completed')
  })

  it('自动分配唯一 ID 和时间戳', () => {
    historyStore.addRecord({
      url: 'https://example.com/a.zip',
      fileName: 'a.zip',
      fileSize: 100,
      status: 'completed',
      duration: 1000,
      avgSpeed: 100,
    })

    const record = historyStore.records[0]
    expect(record?.id).toBeDefined()
    expect(record!.id.length).toBeGreaterThan(0)
    expect(record?.completedAt).toBeDefined()
    expect(typeof record!.completedAt).toBe('string')
  })

  it('记录保存到 localStorage', () => {
    historyStore.addRecord({
      url: 'https://example.com/b.zip',
      fileName: 'b.zip',
      fileSize: 200,
      status: 'failed',
      duration: 3000,
      avgSpeed: 50,
    })

    const stored = localStorage.getItem(STORAGE_KEY)
    expect(stored).toBeTruthy()
    const parsed = JSON.parse(stored!)
    expect(parsed).toHaveLength(1)
    expect(parsed[0].fileName).toBe('b.zip')
  })

  it('从 localStorage 恢复数据', async () => {
    const mockData: HistoryRecord[] = [
      {
        id: 'test-1',
        url: 'https://example.com/old.zip',
        fileName: 'old.zip',
        fileSize: 4096,
        status: 'completed',
        duration: 10000,
        avgSpeed: 409,
        completedAt: '2026-05-30T10:00:00Z',
      },
    ]
    localStorage.setItem(STORAGE_KEY, JSON.stringify(mockData))

    vi.resetModules()
    const mod = await import('../history')
    expect(mod.historyRecords).toHaveLength(1)
    expect(mod.historyRecords[0]?.id).toBe('test-1')
  })

  it('环形缓冲区限制 100 条记录', () => {
    for (let i = 0; i < 105; i++) {
      historyStore.addRecord({
        url: `https://example.com/file${i}.zip`,
        fileName: `file${i}.zip`,
        fileSize: 100,
        status: 'completed',
        duration: 1000,
        avgSpeed: 100,
      })
    }

    expect(historyStore.records).toHaveLength(100)
    expect(historyStore.records[0]?.fileName).toBe('file104.zip')
    expect(historyStore.records[99]?.fileName).toBe('file5.zip')
  })

  it('新记录插入到队列头部', () => {
    historyStore.addRecord({
      url: 'https://example.com/first.zip',
      fileName: 'first.zip',
      fileSize: 100,
      status: 'completed',
      duration: 1000,
      avgSpeed: 100,
    })
    historyStore.addRecord({
      url: 'https://example.com/second.zip',
      fileName: 'second.zip',
      fileSize: 200,
      status: 'failed',
      duration: 2000,
      avgSpeed: 50,
    })

    expect(historyStore.records[0]?.fileName).toBe('second.zip')
    expect(historyStore.records[1]?.fileName).toBe('first.zip')
  })

  it('清除历史记录', () => {
    historyStore.addRecord({
      url: 'https://example.com/c.zip',
      fileName: 'c.zip',
      fileSize: 100,
      status: 'completed',
      duration: 1000,
      avgSpeed: 100,
    })

    historyStore.clearHistory()
    expect(historyStore.records).toHaveLength(0)
    expect(localStorage.getItem(STORAGE_KEY)).toBe('[]')
  })

  it('根据 ID 查找记录', () => {
    historyStore.addRecord({
      url: 'https://example.com/target.zip',
      fileName: 'target.zip',
      fileSize: 100,
      status: 'completed',
      duration: 1000,
      avgSpeed: 100,
    })

    const id = historyStore.records[0]!.id
    const found = historyStore.getRecordById(id)
    expect(found).toBeDefined()
    expect(found!.fileName).toBe('target.zip')

    const notFound = historyStore.getRecordById('non-existent')
    expect(notFound).toBeUndefined()
  })
})

describe('HistoryStore 过滤功能', () => {
  beforeEach(() => {
    const statuses: HistoryRecord['status'][] = ['completed', 'failed', 'cancelled', 'completed', 'failed']
    statuses.forEach((status, i) => {
      historyStore.addRecord({
        url: `https://example.com/${status}${i}.zip`,
        fileName: `${status}${i}.zip`,
        fileSize: 100 * (i + 1),
        status,
        duration: 1000 * (i + 1),
        avgSpeed: 100 * (i + 1),
      })
    })
  })

  it('all 过滤返回所有记录', () => {
    const result = historyStore.getRecords('all')
    expect(result).toHaveLength(5)
  })

  it('completed 过滤只返回已完成', () => {
    const result = historyStore.getRecords('completed')
    expect(result).toHaveLength(2)
    expect(result.every(r => r.status === 'completed')).toBe(true)
  })

  it('failed 过滤只返回失败', () => {
    const result = historyStore.getRecords('failed')
    expect(result).toHaveLength(2)
    expect(result.every(r => r.status === 'failed')).toBe(true)
  })

  it('cancelled 过滤只返回已取消', () => {
    const result = historyStore.getRecords('cancelled')
    expect(result).toHaveLength(1)
    expect(result[0]?.status).toBe('cancelled')
  })
})

describe('HistoryStore 统计计算', () => {
  it('空记录返回零值统计', () => {
    const stats = historyStore.getStats()
    expect(stats.totalDownloads).toBe(0)
    expect(stats.totalBytes).toBe(0)
    expect(stats.avgSpeed).toBe(0)
    expect(stats.successRate).toBe(0)
    expect(stats.totalDuration).toBe(0)
    expect(stats.completedCount).toBe(0)
    expect(stats.failedCount).toBe(0)
    expect(stats.cancelledCount).toBe(0)
  })

  it('统计计算准确', () => {
    historyStore.addRecord({
      url: 'https://example.com/a.zip',
      fileName: 'a.zip',
      fileSize: 1024 * 1024,
      status: 'completed',
      duration: 5000,
      avgSpeed: 204800,
    })
    historyStore.addRecord({
      url: 'https://example.com/b.zip',
      fileName: 'b.zip',
      fileSize: 512 * 1024,
      status: 'failed',
      duration: 2000,
      avgSpeed: 256000,
    })
    historyStore.addRecord({
      url: 'https://example.com/c.zip',
      fileName: 'c.zip',
      fileSize: 2048 * 1024,
      status: 'completed',
      duration: 10000,
      avgSpeed: 204800,
    })
    historyStore.addRecord({
      url: 'https://example.com/d.zip',
      fileName: 'd.zip',
      fileSize: 256 * 1024,
      status: 'cancelled',
      duration: 1500,
      avgSpeed: 100000,
    })

    const stats = historyStore.getStats()

    expect(stats.totalDownloads).toBe(4)
    expect(stats.totalBytes).toBe(1024 * 1024 + 512 * 1024 + 2048 * 1024 + 256 * 1024)
    expect(stats.avgSpeed).toBeCloseTo((204800 + 256000 + 204800 + 100000) / 4, 0)
    expect(stats.successRate).toBeCloseTo(2 / 4, 2)
    expect(stats.totalDuration).toBe(5000 + 2000 + 10000 + 1500)
    expect(stats.completedCount).toBe(2)
    expect(stats.failedCount).toBe(1)
    expect(stats.cancelledCount).toBe(1)
  })

  it('成功率计算正确（无记录时）', () => {
    const stats = historyStore.getStats()
    expect(stats.successRate).toBe(0)
  })

  it('成功率计算正确（全部成功）', () => {
    for (let i = 0; i < 3; i++) {
      historyStore.addRecord({
        url: `https://example.com/s${i}.zip`,
        fileName: `s${i}.zip`,
        fileSize: 100,
        status: 'completed',
        duration: 1000,
        avgSpeed: 100,
      })
    }
    const stats = historyStore.getStats()
    expect(stats.successRate).toBe(1)
  })

  it('成功率计算正确（全部失败）', () => {
    for (let i = 0; i < 3; i++) {
      historyStore.addRecord({
        url: `https://example.com/f${i}.zip`,
        fileName: `f${i}.zip`,
        fileSize: 100,
        status: 'failed',
        duration: 1000,
        avgSpeed: 100,
      })
    }
    const stats = historyStore.getStats()
    expect(stats.successRate).toBe(0)
  })
})

describe('HistoryStore 数据持久化', () => {
  it('localStorage 异常时不崩溃', () => {
    const originalSetItem = localStorage.setItem
    localStorage.setItem = vi.fn(() => {
      throw new Error('QuotaExceeded')
    })

    expect(() => {
      historyStore.addRecord({
        url: 'https://example.com/x.zip',
        fileName: 'x.zip',
        fileSize: 100,
        status: 'completed',
        duration: 1000,
        avgSpeed: 100,
      })
    }).not.toThrow()

    localStorage.setItem = originalSetItem
  })

  it('localStorage 数据损坏时回退到空数组', async () => {
    localStorage.setItem(STORAGE_KEY, 'invalid json{[')

    vi.resetModules()
    const mod = await import('../history')
    expect(mod.historyRecords).toEqual([])
  })
})
