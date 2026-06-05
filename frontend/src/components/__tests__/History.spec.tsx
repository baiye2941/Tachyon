import { describe, it, expect, beforeEach, vi } from 'vitest'
import { render, screen, fireEvent, cleanup } from '@solidjs/testing-library'
import HistoryPanel from '../HistoryPanel'
import StatsDashboard from '../StatsDashboard'
import type { HistoryRecord, HistoryStats } from '../../stores/history'

const makeRecord = (overrides: Partial<HistoryRecord> = {}): HistoryRecord => ({
  id: `id-${Math.random().toString(36).slice(2)}`,
  url: 'https://example.com/file.zip',
  fileName: 'file.zip',
  fileSize: 1024 * 1024,
  status: 'completed',
  duration: 5000,
  avgSpeed: 204800,
  completedAt: '2026-05-30T10:00:00Z',
  ...overrides,
})

describe('HistoryPanel 历史记录面板', () => {
  const records = [
    makeRecord({ fileName: 'a.zip', status: 'completed', fileSize: 1024 * 1024 }),
    makeRecord({ fileName: 'b.zip', status: 'failed', fileSize: 512 * 1024 }),
    makeRecord({ fileName: 'c.zip', status: 'cancelled', fileSize: 256 * 1024 }),
  ]

  beforeEach(() => {
    cleanup()
  })

  it('渲染所有记录', () => {
    render(() => <HistoryPanel records={records} onClear={() => {}} />)
    expect(screen.getByText('a.zip')).toBeDefined()
    expect(screen.getByText('b.zip')).toBeDefined()
    expect(screen.getByText('c.zip')).toBeDefined()
  })

  it('completed 过滤只显示已完成', () => {
    render(() => <HistoryPanel records={records} onClear={() => {}} />)
    const btn = screen.getAllByLabelText('过滤已完成')[0]
    fireEvent.click(btn)
    expect(screen.getByText('a.zip')).toBeDefined()
    expect(screen.queryByText('b.zip')).toBeNull()
    expect(screen.queryByText('c.zip')).toBeNull()
  })

  it('failed 过滤只显示失败', () => {
    render(() => <HistoryPanel records={records} onClear={() => {}} />)
    const btn = screen.getAllByLabelText('过滤失败')[0]
    fireEvent.click(btn)
    expect(screen.queryByText('a.zip')).toBeNull()
    expect(screen.getByText('b.zip')).toBeDefined()
    expect(screen.queryByText('c.zip')).toBeNull()
  })

  it('cancelled 过滤只显示已取消', () => {
    render(() => <HistoryPanel records={records} onClear={() => {}} />)
    const btn = screen.getAllByLabelText('过滤已取消')[0]
    fireEvent.click(btn)
    expect(screen.queryByText('a.zip')).toBeNull()
    expect(screen.queryByText('b.zip')).toBeNull()
    expect(screen.getByText('c.zip')).toBeDefined()
  })

  it('点击过滤按钮切换过滤状态', () => {
    render(() => <HistoryPanel records={records} onClear={() => {}} />)
    // 初始显示全部
    expect(screen.getByText('a.zip')).toBeDefined()
    expect(screen.getByText('b.zip')).toBeDefined()

    // 点击 completed 过滤
    const btn = screen.getAllByLabelText('过滤已完成')[0]
    fireEvent.click(btn)
    expect(screen.getByText('a.zip')).toBeDefined()
    expect(screen.queryByText('b.zip')).toBeNull()

    // 再点 all 恢复全部
    const allBtn = screen.getAllByLabelText('过滤全部')[0]
    fireEvent.click(allBtn)
    expect(screen.getByText('a.zip')).toBeDefined()
    expect(screen.getByText('b.zip')).toBeDefined()
  })

  it('点击清除按钮触发 onClear', () => {
    const onClear = vi.fn()
    render(() => <HistoryPanel records={records} onClear={onClear} />)

    const clearBtn = screen.getAllByLabelText('清除历史')[0]
    fireEvent.click(clearBtn)
    expect(onClear).toHaveBeenCalled()
  })

  it('空记录显示空状态', () => {
    render(() => <HistoryPanel records={[]} onClear={() => {}} />)
    expect(screen.getByText('暂无历史记录')).toBeDefined()
  })

  it('显示文件大小和状态', () => {
    render(() => <HistoryPanel records={records} onClear={() => {}} />)
    expect(screen.getAllByText('1.0 MB').length).toBeGreaterThan(0)
    expect(screen.getAllByText('已完成').length).toBeGreaterThan(0)
    expect(screen.getAllByText('失败').length).toBeGreaterThan(0)
  })
})

describe('StatsDashboard 统计仪表盘', () => {
  const stats: HistoryStats = {
    totalDownloads: 10,
    totalBytes: 1024 * 1024 * 50,
    avgSpeed: 1024 * 1024,
    successRate: 0.8,
    totalDuration: 3600000,
    completedCount: 8,
    failedCount: 1,
    cancelledCount: 1,
  }

  beforeEach(() => {
    cleanup()
  })

  it('渲染所有统计项', () => {
    render(() => <StatsDashboard stats={stats} />)
    expect(screen.getByText('总下载量')).toBeDefined()
    expect(screen.getByText('平均速度')).toBeDefined()
    expect(screen.getByText('成功率')).toBeDefined()
    expect(screen.getByText('总耗时')).toBeDefined()
  })

  it('显示正确的数值', () => {
    render(() => <StatsDashboard stats={stats} />)
    expect(screen.getAllByText('10').length).toBeGreaterThan(0)
    expect(screen.getAllByText('80.0%').length).toBeGreaterThan(0)
  })

  it('零值统计正确显示', () => {
    const zeroStats: HistoryStats = {
      totalDownloads: 0,
      totalBytes: 0,
      avgSpeed: 0,
      successRate: 0,
      totalDuration: 0,
      completedCount: 0,
      failedCount: 0,
      cancelledCount: 0,
    }
    render(() => <StatsDashboard stats={zeroStats} />)
    expect(screen.getAllByText('0').length).toBeGreaterThan(0)
    expect(screen.getByText('0.0%')).toBeDefined()
  })
})
