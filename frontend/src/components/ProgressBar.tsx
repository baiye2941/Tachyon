import type { DownloadStatus } from '../types'

interface ProgressBarProps {
  progress: number
  status: DownloadStatus
  label?: string
}

export default function ProgressBar(props: ProgressBarProps) {
  const fillColor = () => {
    switch (props.status) {
      case 'downloading': return 'bg-accent'
      case 'pending': return 'bg-warning'
      case 'paused': return 'bg-text-secondary'
      case 'completed': return 'bg-success'
      case 'failed': return 'bg-error'
      case 'cancelled': return 'bg-text-tertiary'
      default: return 'bg-text-secondary'
    }
  }

  return (
    <div
      class="h-1 bg-white/5 rounded-full overflow-hidden"
      role="progressbar"
      aria-valuenow={props.progress}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-label={props.label}
    >
      <div class={`h-full rounded-full transition-all duration-300 ${fillColor()}`} style={{ width: `${props.progress}%` }} />
    </div>
  )
}