import type { DownloadStatus } from '../types'
import { statusColor } from '../utils/format'

interface ProgressBarProps {
  progress: number
  status: DownloadStatus
  label?: string
}

export default function ProgressBar(props: ProgressBarProps) {
  const isDownloading = () => props.status === 'downloading'
  const isCompleted = () => props.status === 'completed'

  return (
    <div
      class="h-1 bg-white/5 rounded-full overflow-hidden"
      role="progressbar"
      aria-valuenow={props.progress}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-label={props.label}
    >
      <div
        class={`h-full rounded-full transition-[width] duration-300 ${statusColor(props.status)} ${isDownloading() ? 'progress-striped' : ''} ${isCompleted() ? 'animate-flash' : ''}`}
        style={{ width: `${props.progress}%` }}
      />
    </div>
  )
}
