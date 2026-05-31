import { Show } from 'solid-js'
import type { TaskInfo } from '../types'
import ProgressBar from './ProgressBar'
import { formatSize, formatSpeed, guessExt, statusText, statusClass } from '../utils/format'
import { IconPause, IconResume, IconCancel, IconDelete } from '../utils/icons'

interface DownloadCardProps {
  task: TaskInfo
  selected: boolean
  onSelect: (id: string) => void
  onPause: (id: string) => void
  onResume: (id: string) => void
  onCancel: (id: string) => void
  onDelete: (id: string) => void
}

export default function DownloadCard(props: DownloadCardProps) {
  return (
    <div
      class="bg-surface border border-border rounded-lg p-3 cursor-pointer transition-all duration-150 hover:border-border-hover"
      classList={{ 'border-accent/30 bg-accent-muted': props.selected }}
      onClick={() => props.onSelect(props.task.id)}
    >
      <div class="flex items-center justify-between mb-2">
        <div class="flex items-center gap-2 min-w-0 text-[13px] font-medium">
          <span class="truncate">
            {props.task.fileName}
          </span>
          <span class="text-[10px] uppercase text-text-tertiary bg-white/5 px-1.5 py-0.5 rounded shrink-0">{guessExt(props.task.fileName)}</span>
        </div>
        <span class={`text-[11px] font-semibold px-2 py-0.5 rounded-full ${statusClass(props.task.status)}`}>
          {statusText(props.task.status)}
        </span>
      </div>

      <ProgressBar progress={props.task.progress} status={props.task.status} />

      <div class="flex items-center gap-3 mt-2 text-[11px]">
        <span class="font-mono">{props.task.progress.toFixed(1)}%</span>
        <span class="flex items-center gap-1">
          <span class="text-text-tertiary">大小</span>
          <span class="font-mono">{formatSize(props.task.fileSize)}</span>
        </span>
        <Show when={props.task.speed > 0}>
          <span class="flex items-center gap-1">
            <span class="text-text-tertiary">速度</span>
            <span class="font-mono text-accent">{formatSpeed(props.task.speed)}</span>
          </span>
        </Show>
        <span class="flex items-center gap-1">
          <span class="text-text-tertiary">分片</span>
          <span class="font-mono">{props.task.fragmentsDone}/{props.task.fragmentsTotal}</span>
        </span>
      </div>

      <Show when={props.task.status === 'downloading' || props.task.status === 'paused' || props.task.status === 'pending'}>
        <div class="flex items-center gap-2 mt-2 pt-2 border-t border-white/5">
          <Show when={props.task.status === 'downloading' || props.task.status === 'pending'}>
            <button
              class="flex items-center gap-1 px-2 py-1 text-[11px] rounded bg-white/5 text-text-secondary hover:bg-white/10 hover:text-text-primary transition-colors duration-100"
              onClick={(e) => { e.stopPropagation(); props.onPause(props.task.id) }}
              aria-label={`暂停 ${props.task.fileName}`}
            >
              <IconPause /> 暂停
            </button>
          </Show>
          <Show when={props.task.status === 'paused'}>
            <button
              class="flex items-center gap-1 px-2 py-1 text-[11px] rounded bg-white/5 text-text-secondary hover:bg-white/10 hover:text-text-primary transition-colors duration-100"
              onClick={(e) => { e.stopPropagation(); props.onResume(props.task.id) }}
              aria-label={`恢复 ${props.task.fileName}`}
            >
              <IconResume /> 恢复
            </button>
          </Show>
          <Show when={props.task.status !== 'cancelled'}>
            <button
              class="flex items-center gap-1 px-2 py-1 text-[11px] rounded bg-white/5 text-text-secondary hover:bg-white/10 hover:text-text-primary transition-colors duration-100"
              onClick={(e) => { e.stopPropagation(); props.onCancel(props.task.id) }}
              aria-label={`取消 ${props.task.fileName}`}
            >
              <IconCancel /> 取消
            </button>
          </Show>
        </div>
      </Show>

      <Show when={props.task.status === 'completed' || props.task.status === 'failed' || props.task.status === 'cancelled'}>
        <div class="flex items-center gap-2 mt-2 pt-2 border-t border-white/5">
          <button
            class="flex items-center gap-1 px-2 py-1 text-[11px] rounded bg-white/5 text-text-secondary hover:bg-white/10 hover:text-error transition-colors duration-100"
            onClick={(e) => { e.stopPropagation(); props.onDelete(props.task.id) }}
            aria-label={`删除 ${props.task.fileName}`}
          >
            <IconDelete /> 删除
          </button>
        </div>
      </Show>
    </div>
  )
}