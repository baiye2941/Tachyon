import { createSignal, For, Show } from 'solid-js'
import type { HistoryRecord, HistoryFilter } from '../stores/history'
import { formatSize, statusText, statusClass } from '../utils/format'
import { Icon } from '../utils/icons'

interface HistoryPanelProps {
  records: HistoryRecord[]
  onClear: () => void
}

const filterLabels: Record<HistoryFilter, string> = {
  all: '全部',
  completed: '已完成',
  failed: '失败',
  cancelled: '已取消',
}

export default function HistoryPanel(props: HistoryPanelProps) {
  const [filter, setFilter] = createSignal<HistoryFilter>('all')

  const filtered = () => {
    if (filter() === 'all') return props.records
    return props.records.filter(r => r.status === filter())
  }

  return (
    <div class="flex flex-col gap-3">
      <div class="flex items-center justify-between">
        <div class="flex items-center gap-1.5">
          <For each={Object.keys(filterLabels) as HistoryFilter[]}>
            {(f) => (
              <button
                class={`px-2.5 py-1 text-[10px] rounded transition-colors duration-100 ${
                  filter() === f
                    ? 'bg-accent text-canvas font-medium'
                    : 'bg-white/5 text-text-secondary hover:bg-white/10 hover:text-text-primary'
                }`}
                onClick={() => setFilter(f)}
                aria-label={`过滤${filterLabels[f]}`}
              >
                {filterLabels[f]}
              </button>
            )}
          </For>
        </div>
        <button
          class="px-2.5 py-1 text-[10px] rounded bg-white/5 text-text-secondary hover:bg-error/10 hover:text-error transition-colors duration-100"
          onClick={() => props.onClear()}
          aria-label="清除历史"
        >
          清除历史
        </button>
      </div>

      <Show
        when={filtered().length > 0}
        fallback={
          <div class="flex flex-col items-center justify-center gap-2 py-8 text-text-tertiary">
            <Icon name="clock" class="w-12 h-12 text-text-tertiary mx-auto" />
            <div class="text-[12px]">暂无历史记录</div>
            <div class="text-[11px] text-text-tertiary">完成的下载任务将自动记录在此</div>
          </div>
        }
      >
        <div class="glass-panel rounded-lg overflow-hidden flex flex-col">
          <For each={filtered()}>
            {(record) => (
              <div class="flex items-center gap-3 px-3 py-2 border-b border-white/5 hover:bg-white/[0.03] transition-colors duration-150">
                <span class="truncate text-[12px] font-medium flex-1 min-w-0 text-text-primary">
                  {record.fileName}
                </span>
                <span class={`text-[10px] font-semibold px-1.5 py-0.5 rounded-full shrink-0 ${statusClass(record.status)}`}>
                  {statusText(record.status)}
                </span>
                <span class="text-[10px] font-mono text-text-secondary shrink-0">{formatSize(record.fileSize)}</span>
                <span class="text-[10px] font-mono text-text-tertiary shrink-0">{(record.avgSpeed / 1024).toFixed(1)} KB/s</span>
                <span class="text-[10px] font-mono text-text-tertiary shrink-0">{(record.duration / 1000).toFixed(1)}s</span>
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  )
}
