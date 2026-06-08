import { createSignal, createMemo, For, Show } from 'solid-js'
import type { TaskInfo } from '../types'
import {
  CloseIcon, HistoryIcon, FolderOpenIcon, RefreshIcon, TrashIcon,
  TrophyIcon, PackageIcon,
} from './icons'
import { formatSize, formatSpeed } from '../utils/format'

interface HistoryPanelProps {
  visible: boolean
  tasks: TaskInfo[]
  onClose: () => void
  onOpenFolder: (taskId: string) => void
  onRedownload: (task: TaskInfo) => void
  onDeleteRecord: (taskId: string) => void
}

export default function HistoryPanel(props: HistoryPanelProps) {
  const [timeRange, setTimeRange] = createSignal<'7d' | '30d' | 'all'>('all')
  const [searchQuery, setSearchQuery] = createSignal('')

  const completedTasks = createMemo(() => {
    return props.tasks.filter(t => t.status === 'completed')
  })

  const filteredTasks = createMemo(() => {
    let result = completedTasks()
    const sq = searchQuery().trim().toLowerCase()
    if (sq) {
      result = result.filter(t => t.fileName.toLowerCase().includes(sq))
    }
    return result
  })

  const stats = createMemo(() => {
    const tasks = completedTasks()
    const totalDownloaded = tasks.reduce((sum, t) => sum + (t.fileSize || 0), 0)
    const totalCount = tasks.length
    const avgSpeed = tasks.length > 0
      ? tasks.reduce((sum, t) => sum + t.speed, 0) / tasks.length
      : 0
    const maxFile = tasks.length > 0
      ? tasks.reduce((max, t) => (t.fileSize || 0) > (max.fileSize || 0) ? t : max, tasks[0])
      : null
    const maxSpeed = tasks.length > 0
      ? Math.max(...tasks.map(t => t.speed))
      : 0
    return { totalDownloaded, totalCount, avgSpeed, maxFile, maxSpeed }
  })

  function timeAgo(dateStr: string): string {
    const diff = Date.now() - new Date(dateStr).getTime()
    const days = Math.floor(diff / (1000 * 60 * 60 * 24))
    if (days === 0) return '今天'
    if (days === 1) return '昨天'
    if (days < 7) return `${days}天前`
    if (days < 30) return `${Math.floor(days / 7)}周前`
    return `${Math.floor(days / 30)}月前`
  }

  const trendData = createMemo(() => {
    const days = 30
    const data: { day: string; size: number }[] = []
    for (let i = days - 1; i >= 0; i--) {
      const date = new Date(Date.now() - i * 24 * 60 * 60 * 1000)
      const dayStr = date.toISOString().slice(0, 10)
      const dayTasks = completedTasks().filter(t => t.createdAt.startsWith(dayStr))
      const size = dayTasks.reduce((sum, t) => sum + (t.fileSize || 0), 0)
      data.push({ day: dayStr.slice(5), size })
    }
    return data
  })

  const maxTrendSize = createMemo(() => {
    return Math.max(...trendData().map(d => d.size), 1)
  })

  return (
    <div
      class="slide-panel"
      style={{
        width: '480px',
        transform: props.visible ? 'translateX(0)' : 'translateX(100%)',
        overflow: 'hidden',
      }}
    >
      {/* Header */}
      <div class="panel-header">
        <div class="panel-title">
          <HistoryIcon />
          <span>实验数据</span>
        </div>
        <div class="flex items-center gap-2">
          <For each={(['7d', '30d', 'all'] as const)}>
            {(range) => (
              <button
                class={timeRange() === range ? 'pill-btn pill-btn-active' : 'pill-btn pill-btn-default'}
                onClick={() => setTimeRange(range)}
              >
                {range === '7d' ? '近7天' : range === '30d' ? '近30天' : '全部'}
              </button>
            )}
          </For>
          <button
            class="icon-btn-sm hover-light"
            onClick={() => props.onClose()}
            aria-label="关闭历史面板"
          >
            <CloseIcon />
          </button>
        </div>
      </div>

      <div class="flex-1 overflow-y-auto" style={{ padding: '20px' }}>
        {/* Stats Grid */}
        <div style={{
          display: 'grid',
          'grid-template-columns': 'repeat(3, 1fr)',
          gap: '12px',
          'margin-bottom': '20px',
        }}>
          <div class="glass" style={{ padding: '16px', 'border-radius': '10px', 'text-align': 'center' }}>
            <div class="mono" style={{ 'font-size': '20px', 'font-weight': 700, color: '#F0F0F5' }}>
              {formatSize(stats().totalDownloaded)}
            </div>
            <div style={{ 'font-size': '11px', color: '#6B7280', 'margin-top': '4px' }}>总下载量</div>
          </div>
          <div class="glass" style={{ padding: '16px', 'border-radius': '10px', 'text-align': 'center' }}>
            <div class="mono" style={{ 'font-size': '20px', 'font-weight': 700, color: '#00B4D8' }}>
              {stats().totalCount}
            </div>
            <div style={{ 'font-size': '11px', color: '#6B7280', 'margin-top': '4px' }}>任务总数</div>
          </div>
          <div class="glass" style={{ padding: '16px', 'border-radius': '10px', 'text-align': 'center' }}>
            <div class="mono" style={{ 'font-size': '20px', 'font-weight': 700, color: '#F0F0F5' }}>
              {formatSpeed(stats().avgSpeed)}
            </div>
            <div style={{ 'font-size': '11px', color: '#6B7280', 'margin-top': '4px' }}>平均速度</div>
          </div>
          <div class="glass" style={{ padding: '16px', 'border-radius': '10px', 'text-align': 'center' }}>
            <div class="mono" style={{ 'font-size': '20px', 'font-weight': 700, color: '#00D4AA' }}>
              {formatSpeed(stats().maxSpeed)}
            </div>
            <div style={{ 'font-size': '11px', color: '#6B7280', 'margin-top': '4px' }}>最快记录</div>
          </div>
          <div class="glass" style={{ padding: '16px', 'border-radius': '10px', 'text-align': 'center' }}>
            <div class="mono" style={{ 'font-size': '20px', 'font-weight': 700, color: '#00D4AA' }}>
              {stats().totalCount > 0 ? '100%' : '0%'}
            </div>
            <div style={{ 'font-size': '11px', color: '#6B7280', 'margin-top': '4px' }}>成功率</div>
          </div>
          <div class="glass" style={{ padding: '16px', 'border-radius': '10px', 'text-align': 'center' }}>
            <div class="truncate mono" style={{ 'font-size': '14px', 'font-weight': 700, color: '#F59E0B' }}>
              {stats().maxFile ? formatSize(stats().maxFile!.fileSize || 0) : '-'}
            </div>
            <div style={{ 'font-size': '11px', color: '#6B7280', 'margin-top': '4px' }}>最大文件</div>
          </div>
        </div>

        {/* Trend Chart */}
        <div class="glass" style={{ padding: '16px', 'border-radius': '10px', 'margin-bottom': '20px' }}>
          <div style={{ 'font-size': '12px', 'font-weight': 600, color: '#6B7280', 'margin-bottom': '12px' }}>下载量趋势</div>
          <div class="flex items-end gap-1" style={{ height: '120px' }}>
            <For each={trendData()}>
              {(item) => {
                const height = () => item.size > 0 ? Math.max((item.size / maxTrendSize()) * 100, 1) : 1
                return (
                  <div class="flex-1 flex flex-col items-center gap-1 group" style={{ height: '100%', 'justify-content': 'flex-end' }}>
                    <div
                      style={{
                        width: '100%',
                        height: `${height()}%`,
                        'min-height': item.size > 0 ? '2px' : '1px',
                        'border-radius': '2px 2px 0 0',
                        background: item.size > 0
                          ? 'linear-gradient(to top, #00D4AA, #00B4D8)'
                          : 'rgba(255,255,255,0.05)',
                        transition: 'height 300ms ease-out',
                      }}
                    />
                  </div>
                )
              }}
            </For>
          </div>
        </div>

        {/* Fun Facts */}
        <Show when={stats().maxFile}>
          <div class="glass" style={{ padding: '14px', 'border-radius': '8px', 'margin-bottom': '20px' }}>
            <div class="flex items-center gap-2" style={{ color: '#00D4AA' }}>
              <TrophyIcon />
              <span style={{ 'font-size': '12px', color: '#6B7280' }}>最快记录</span>
            </div>
            <div class="mono" style={{ 'font-size': '14px', color: '#F0F0F5', 'margin-top': '4px' }}>
              {formatSpeed(stats().maxSpeed)} — {stats().maxFile?.fileName}
            </div>
          </div>
        </Show>

        {/* Records */}
        <div class="section-label" style={{ 'margin-bottom': '12px' }}>历史记录</div>
        <input
          type="text"
          placeholder="搜索历史记录..."
          value={searchQuery()}
          onInput={e => setSearchQuery(e.currentTarget.value)}
          class="input"
          style={{ width: '100%', 'margin-bottom': '12px', 'font-size': '13px' }}
        />
        <Show
          when={filteredTasks().length > 0}
          fallback={<div style={{ color: '#6B7280', 'font-size': '13px' }}>暂无历史记录</div>}
        >
          <For each={filteredTasks()}>
            {(task) => (
              <div
                class="flex items-center gap-3 hover-row"
                style={{
                  padding: '10px 12px',
                  'border-radius': '8px',
                  transition: 'all 150ms ease',
                }}
              >
                <PackageIcon />
                <div class="flex-1 min-w-0">
                  <div class="truncate" style={{ 'font-size': '14px', color: '#F0F0F5' }}>{task.fileName}</div>
                  <div style={{ 'font-size': '12px', color: '#6B7280' }}>
                    {formatSize(task.fileSize || 0)} · 已完成 · {timeAgo(task.createdAt)}
                  </div>
                </div>
                <div class="flex items-center gap-1">
                  <button
                    class="icon-btn-sm hover-light"
                    onClick={() => props.onOpenFolder(task.id)}
                    aria-label={`打开目录 ${task.fileName}`}
                  >
                    <FolderOpenIcon />
                  </button>
                  <button
                    class="icon-btn-sm hover-light"
                    onClick={() => props.onRedownload(task)}
                    aria-label={`重新下载 ${task.fileName}`}
                  >
                    <RefreshIcon />
                  </button>
                  <button
                    class="icon-btn-sm hover-danger"
                    onClick={() => props.onDeleteRecord(task.id)}
                    aria-label={`删除记录 ${task.fileName}`}
                  >
                    <TrashIcon />
                  </button>
                </div>
              </div>
            )}
          </For>
        </Show>
      </div>
    </div>
  )
}
