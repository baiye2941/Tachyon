import { Show } from 'solid-js'
import type { TaskInfo } from '../types'
import ProgressBar from './ProgressBar'
import { formatSize, formatSpeed, guessExt, statusText } from '../utils/format'
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
      class="download-card"
      classList={{ selected: props.selected }}
      onClick={() => props.onSelect(props.task.id)}
    >
      <div class="card-header">
        <div class="card-name">
          <span style={{ overflow: 'hidden', 'text-overflow': 'ellipsis', 'white-space': 'nowrap' as const }}>
            {props.task.file_name}
          </span>
          <span class="ext">{guessExt(props.task.file_name)}</span>
        </div>
        <span class={`card-status status-${props.task.status}`}>
          {statusText(props.task.status)}
        </span>
      </div>

      <ProgressBar progress={props.task.progress} status={props.task.status} />

      <div class="card-details">
        <span class="detail-item mono">{props.task.progress.toFixed(1)}%</span>
        <span class="detail-item">
          <span class="detail-label">大小</span>
          <span class="mono">{formatSize(props.task.file_size)}</span>
        </span>
        <Show when={props.task.speed > 0}>
          <span class="detail-item">
            <span class="detail-label">速度</span>
            <span class="speed-value">{formatSpeed(props.task.speed)}</span>
          </span>
        </Show>
        <span class="detail-item">
          <span class="detail-label">分片</span>
          <span class="mono">{props.task.fragments_done}/{props.task.fragments_total}</span>
        </span>
      </div>

      <Show when={props.task.status === 'downloading' || props.task.status === 'paused' || props.task.status === 'pending'}>
        <div class="card-actions">
          <Show when={props.task.status === 'downloading' || props.task.status === 'pending'}>
            <button
              class="btn-card-action"
              onClick={(e) => { e.stopPropagation(); props.onPause(props.task.id) }}
            >
              <IconPause /> 暂停
            </button>
          </Show>
          <Show when={props.task.status === 'paused'}>
            <button
              class="btn-card-action"
              onClick={(e) => { e.stopPropagation(); props.onResume(props.task.id) }}
            >
              <IconResume /> 恢复
            </button>
          </Show>
          <Show when={props.task.status !== 'cancelled'}>
            <button
              class="btn-card-action"
              onClick={(e) => { e.stopPropagation(); props.onCancel(props.task.id) }}
            >
              <IconCancel /> 取消
            </button>
          </Show>
        </div>
      </Show>

      <Show when={props.task.status === 'completed' || props.task.status === 'failed' || props.task.status === 'cancelled'}>
        <div class="card-actions">
          <button
            class="btn-card-action action-danger"
            onClick={(e) => { e.stopPropagation(); props.onDelete(props.task.id) }}
          >
            <IconDelete /> 删除
          </button>
        </div>
      </Show>
    </div>
  )
}
