import { For, Show } from 'solid-js'
import type { HistoryStats } from '../stores/history'
import { formatSize, formatSpeed } from '../utils/format'

interface StatsDashboardProps {
  stats: HistoryStats
}

interface StatItem {
  label: string
  value: string
  subValue?: string
}

function formatDuration(ms: number): string {
  if (ms === 0) return '0s'
  const seconds = Math.floor(ms / 1000)
  const hours = Math.floor(seconds / 3600)
  const minutes = Math.floor((seconds % 3600) / 60)
  const secs = seconds % 60

  const parts: string[] = []
  if (hours > 0) parts.push(`${hours}h`)
  if (minutes > 0) parts.push(`${minutes}m`)
  if (secs > 0 || parts.length === 0) parts.push(`${secs}s`)

  return parts.join(' ')
}

export default function StatsDashboard(props: StatsDashboardProps) {
  const statItems = (): StatItem[] => [
    {
      label: '总下载量',
      value: String(props.stats.totalDownloads),
      subValue: formatSize(props.stats.totalBytes),
    },
    {
      label: '平均速度',
      value: formatSpeed(props.stats.avgSpeed),
    },
    {
      label: '成功率',
      value: `${(props.stats.successRate * 100).toFixed(1)}%`,
      subValue: `${props.stats.completedCount} / ${props.stats.totalDownloads}`,
    },
    {
      label: '总耗时',
      value: formatDuration(props.stats.totalDuration),
    },
  ]

  return (
    <div class="grid grid-cols-2 gap-3">
      <For each={statItems()}>
        {(item) => (
          <div class="bg-surface-elevated rounded-lg p-4 flex flex-col">
            <span class="text-[10px] text-text-tertiary uppercase">{item.label}</span>
            <span class="text-[20px] font-mono font-semibold text-text-primary mt-1">{item.value}</span>
            <Show when={item.subValue}>
              <span class="text-[11px] font-mono text-text-secondary mt-1">{item.subValue}</span>
            </Show>
          </div>
        )}
      </For>
    </div>
  )
}
