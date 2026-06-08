import { describe, it, expect, beforeEach, vi } from 'vitest'
import { render, screen, fireEvent, cleanup } from '@solidjs/testing-library'
import HistoryPanel from '../HistoryPanel'
import StatsDashboard from '../StatsDashboard'
import type { TaskInfo } from '../../types'
import type { HistoryStats } from '../../stores/history'

const makeTask = (overrides: Partial<TaskInfo> = {}): TaskInfo => ({
  id: `id-${Math.random().toString(36).slice(2)}`,
  url: 'https://example.com/file.zip',
  fileName: 'file.zip',
  fileSize: 1024 * 1024,
  downloaded: 1024 * 1024,
  progress: 1,
  speed: 204800,
  status: 'completed',
  fragmentsTotal: 4,
  fragmentsDone: 4,
  createdAt: '2026-05-30T10:00:00Z',
  ...overrides,
})

describe('HistoryPanel 历史记录面板', () => {
  const tasks = [
    makeTask({ id: 'a', fileName: 'a.zip', status: 'completed', fileSize: 1024 * 1024 }),
    makeTask({ id: 'b', fileName: 'b.zip', status: 'failed', fileSize: 512 * 1024, progress: 0.2 }),
    makeTask({ id: 'c', fileName: 'c.zip', status: 'completed', fileSize: 256 * 1024 }),
  ]

  beforeEach(() => {
    cleanup()
  })

  const renderPanel = (overrides: Partial<Parameters<typeof HistoryPanel>[0]> = {}) => render(() => (
    <HistoryPanel
      visible={true}
      tasks={tasks}
      onClose={() => {}}
      onOpenFolder={() => {}}
      onRedownload={() => {}}
      onDeleteRecord={() => {}}
      {...overrides}
    />
  ))

  it('只渲染已完成任务记录', () => {
    renderPanel()
    expect(screen.getByText('a.zip')).toBeDefined()
    expect(screen.getByText('c.zip')).toBeDefined()
    expect(screen.queryByText('b.zip')).toBeNull()
  })

  it('搜索历史记录按文件名过滤', () => {
    renderPanel()
    fireEvent.input(screen.getByPlaceholderText('搜索历史记录...'), { target: { value: 'a.' } })
    expect(screen.getByText('a.zip')).toBeDefined()
    expect(screen.queryByText('c.zip')).toBeNull()
  })

  it('点击删除记录触发 onDeleteRecord', () => {
    const onDeleteRecord = vi.fn()
    renderPanel({ onDeleteRecord })
    fireEvent.click(screen.getByLabelText('删除记录 a.zip'))
    expect(onDeleteRecord).toHaveBeenCalledWith('a')
  })

  it('点击重新下载触发 onRedownload 并传回任务', () => {
    const onRedownload = vi.fn()
    renderPanel({ onRedownload })
    fireEvent.click(screen.getByLabelText('重新下载 a.zip'))
    expect(onRedownload).toHaveBeenCalledWith(expect.objectContaining({ id: 'a', fileName: 'a.zip' }))
  })

  it('点击打开目录触发 onOpenFolder', () => {
    const onOpenFolder = vi.fn()
    renderPanel({ onOpenFolder })
    fireEvent.click(screen.getByLabelText('打开目录 a.zip'))
    expect(onOpenFolder).toHaveBeenCalledWith('a')
  })

  it('没有已完成任务时显示空状态', () => {
    renderPanel({ tasks: [makeTask({ id: 'failed', status: 'failed', fileName: 'failed.zip' })] })
    expect(screen.getByText('暂无历史记录')).toBeDefined()
  })

  it('显示文件大小和已完成状态', () => {
    renderPanel()
    expect(screen.getAllByText('1.0 MB').length).toBeGreaterThan(0)
    expect(screen.getAllByText(/已完成/).length).toBeGreaterThan(0)
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
