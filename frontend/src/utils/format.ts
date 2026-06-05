export function formatSize(bytes: number | null): string {
  if (bytes == null || bytes === 0) return '0 B'
  const k = 1024
  const sizes = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.floor(Math.log(bytes) / Math.log(k))
  return (bytes / Math.pow(k, i)).toFixed(i > 1 ? 1 : 0) + ' ' + sizes[i]
}

export function formatSpeed(bytes: number): string {
  return formatSize(bytes) + '/s'
}

export function statusText(status: string): string {
  const map: Record<string, string> = {
    pending: '等待中',
    connecting: '连接中',
    downloading: '下载中',
    paused: '已暂停',
    resuming: '恢复中',
    verifying: '校验中',
    completed: '已完成',
    failed: '失败',
    cancelled: '已取消',
  }
  return map[status] || status
}

export function guessExt(name: string): string {
  const parts = name.split('.')
  return parts.length > 1 ? parts.pop()!.toUpperCase().slice(0, 4) : 'FILE'
}

export type ColorVariant = 'fill' | 'badge'

// 统一状态颜色映射。fill 返回 bg-* 填充色，badge 返回 text-+bg- 组合
export function statusColor(status: string, variant: ColorVariant = 'fill'): string {
  if (variant === 'badge') return statusClass(status)
  const map: Record<string, string> = {
    downloading: 'bg-accent',
    connecting: 'bg-accent',
    resuming: 'bg-accent',
    verifying: 'bg-accent',
    pending: 'bg-warning',
    paused: 'bg-text-secondary',
    completed: 'bg-success',
    failed: 'bg-error',
    cancelled: 'bg-text-tertiary',
  }
  return map[status] || 'bg-text-secondary'
}

export function statusClass(status: string): string {
  const map: Record<string, string> = {
    pending: 'text-warning bg-warning/10',
    connecting: 'text-accent bg-accent/10',
    downloading: 'text-accent bg-accent/10',
    paused: 'text-text-secondary bg-white/5',
    resuming: 'text-accent bg-accent/10',
    verifying: 'text-accent bg-accent/10',
    completed: 'text-success bg-success/10',
    failed: 'text-error bg-error/10',
    cancelled: 'text-text-tertiary bg-white/5',
  }
  return map[status] || 'text-text-secondary bg-white/5'
}
