import { createStore } from 'solid-js/store'

export type HistoryFilter = 'all' | 'completed' | 'failed' | 'cancelled'

export interface HistoryRecord {
  id: string
  url: string
  fileName: string
  fileSize: number
  status: 'completed' | 'failed' | 'cancelled'
  duration: number
  avgSpeed: number
  completedAt: string
}

export interface HistoryStats {
  totalDownloads: number
  totalBytes: number
  avgSpeed: number
  successRate: number
  totalDuration: number
  completedCount: number
  failedCount: number
  cancelledCount: number
}

const STORAGE_KEY = 'tachyon:download_history'
const MAX_RECORDS = 100

function generateId(): string {
  return `${Date.now()}-${Math.random().toString(36).slice(2, 9)}`
}

function loadFromStorage(): HistoryRecord[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return []
    const parsed = JSON.parse(raw)
    if (!Array.isArray(parsed)) return []
    return parsed as HistoryRecord[]
  } catch {
    return []
  }
}

function saveToStorage(records: HistoryRecord[]) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(records))
  } catch {
    // ignore storage errors (e.g. quota exceeded)
  }
}

const [historyRecords, setHistoryRecords] = createStore<HistoryRecord[]>(loadFromStorage())

export { historyRecords }

export function addHistoryRecord(
  record: Omit<HistoryRecord, 'id' | 'completedAt'>
): void {
  const newRecord: HistoryRecord = {
    ...record,
    id: generateId(),
    completedAt: new Date().toISOString(),
  }
  setHistoryRecords(prev => {
    const updated = [newRecord, ...prev]
    if (updated.length > MAX_RECORDS) {
      const trimmed = updated.slice(0, MAX_RECORDS)
      saveToStorage(trimmed)
      return trimmed
    }
    saveToStorage(updated)
    return updated
  })
}

export function getHistoryRecords(filter: HistoryFilter = 'all'): HistoryRecord[] {
  if (filter === 'all') return [...historyRecords]
  return historyRecords.filter(r => r.status === filter)
}

// 单次遍历统计，替代原来 3 次 reduce + 3 次 filter
export function getHistoryStats(): HistoryStats {
  const records = historyRecords
  let totalBytes = 0
  let totalDuration = 0
  let speedSum = 0
  let completedCount = 0
  let failedCount = 0
  let cancelledCount = 0

  for (let i = 0; i < records.length; i++) {
    const r = records[i]
    totalBytes += r.fileSize || 0
    totalDuration += r.duration || 0
    speedSum += r.avgSpeed || 0
    if (r.status === 'completed') completedCount++
    else if (r.status === 'failed') failedCount++
    else if (r.status === 'cancelled') cancelledCount++
  }

  const totalDownloads = records.length
  const avgSpeed = totalDownloads > 0 ? speedSum / totalDownloads : 0
  const successRate = totalDownloads > 0 ? completedCount / totalDownloads : 0

  return {
    totalDownloads,
    totalBytes,
    avgSpeed,
    successRate,
    totalDuration,
    completedCount,
    failedCount,
    cancelledCount,
  }
}

export function clearHistory(): void {
  setHistoryRecords([])
  saveToStorage([])
}

export function getRecordById(id: string): HistoryRecord | undefined {
  return historyRecords.find(r => r.id === id)
}
