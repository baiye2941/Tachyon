import { Show } from 'solid-js'
import type { TaskInfo } from '../types'
import { formatSize, formatSpeed, statusColor } from '../utils/format'
import { IconPause, IconResume, IconCancel, IconDelete } from '../utils/icons'

interface DownloadCardProps {
  task: TaskInfo
  selected: boolean
  onSelect: (id: string) => void
  onToggleSelect?: (id: string) => void
  onPause: (id: string) => void
  onResume: (id: string) => void
  onCancel: (id: string) => void
  onDelete: (id: string) => void
}

const btnClass = 'w-5 h-5 flex items-center justify-center rounded text-text-tertiary hover:bg-white/[0.06] hover:text-text-primary transition-colors duration-150'

export default function DownloadCard(props: DownloadCardProps) {
  const isDownloading = () => props.task.status === 'downloading'

  return (
    <div
      class="group flex items-center gap-2 px-2 py-1 border-b border-border cursor-pointer transition-colors duration-100 hover:bg-[var(--color-surface-hover)]"
      classList={{
        'border-l-2 border-accent bg-accent-muted': props.selected,
      }}
      onClick={() => props.onSelect(props.task.id)}
    >
      <input
        type="checkbox"
        checked={props.selected}
        onClick={(e) => {
          e.stopPropagation()
          if (props.onToggleSelect) {
            props.onToggleSelect(props.task.id)
          } else {
            props.onSelect(props.task.id)
          }
        }}
        class="w-3 h-3 rounded-sm border-border bg-transparent text-accent cursor-pointer shrink-0"
        aria-label={`选择 ${props.task.fileName}`}
      />

      {/* 状态指示点 */}
      <div
        class={`w-2 h-2 rounded-full shrink-0 ${statusColor(props.task.status)}`}
        aria-hidden="true"
      />

      <span class="truncate flex-1 text-[12px] font-medium text-text-primary min-w-0">
        {props.task.fileName}
      </span>

      {/* 进度条 */}
      <div class="w-[100px] h-1 rounded-full bg-white/5 shrink-0 overflow-hidden">
        <div
          class={`h-full rounded-full transition-[width] duration-300 ${statusColor(props.task.status)} ${isDownloading() ? 'progress-striped' : ''}`}
          style={{ width: `${props.task.progress}%` }}
        />
      </div>

      <span class="font-mono text-[11px] w-[42px] text-right text-text-secondary shrink-0">
        {props.task.progress.toFixed(1)}%
      </span>

      <span class="font-mono text-[11px] w-[60px] text-right text-text-tertiary shrink-0">
        {formatSize(props.task.fileSize)}
      </span>

      <Show when={isDownloading() && props.task.speed > 0}>
        <span class="font-mono text-[11px] w-[80px] text-right text-accent shrink-0">
          {formatSpeed(props.task.speed)}
        </span>
      </Show>
      <Show when={!(isDownloading() && props.task.speed > 0)}>
        <span class="font-mono text-[11px] w-[80px] text-right text-accent shrink-0 invisible">
          -
        </span>
      </Show>

      <span class="font-mono text-[10px] w-[36px] text-right text-text-tertiary shrink-0">
        {props.task.fragmentsDone}/{props.task.fragmentsTotal}
      </span>

      {/* 操作按钮 — hover 时才显示 */}
      <div class="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity duration-100 shrink-0">
        <Show when={props.task.status === 'downloading' || props.task.status === 'pending'}>
          <button
            class={btnClass}
            onClick={(e) => { e.stopPropagation(); props.onPause(props.task.id) }}
            aria-label={`暂停 ${props.task.fileName}`}
            title="暂停"
          >
            <IconPause />
          </button>
        </Show>

        <Show when={props.task.status === 'paused'}>
          <button
            class={btnClass}
            onClick={(e) => { e.stopPropagation(); props.onResume(props.task.id) }}
            aria-label={`恢复 ${props.task.fileName}`}
            title="恢复"
          >
            <IconResume />
          </button>
        </Show>

        <Show when={props.task.status !== 'cancelled' && props.task.status !== 'completed' && props.task.status !== 'failed'}>
          <button
            class={btnClass}
            onClick={(e) => { e.stopPropagation(); props.onCancel(props.task.id) }}
            aria-label={`取消 ${props.task.fileName}`}
            title="取消"
          >
            <IconCancel />
          </button>
        </Show>

        <Show when={props.task.status === 'completed' || props.task.status === 'failed' || props.task.status === 'cancelled'}>
          <button
            class="w-5 h-5 flex items-center justify-center rounded text-text-tertiary hover:bg-white/[0.06] hover:text-error transition-colors duration-150"
            onClick={(e) => { e.stopPropagation(); props.onDelete(props.task.id) }}
            aria-label={`删除 ${props.task.fileName}`}
            title="删除"
          >
            <IconDelete />
          </button>
        </Show>
      </div>
    </div>
  )
}
