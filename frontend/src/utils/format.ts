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
    downloading: '下载中',
    completed: '已完成',
    paused: '已暂停',
    failed: '失败',
    pending: '等待中',
    cancelled: '已取消',
  }
  return map[status] || status
}

export function guessExt(name: string): string {
  const parts = name.split('.')
  return parts.length > 1 ? parts.pop()!.toUpperCase().slice(0, 4) : 'FILE'
}

export function statusClass(status: string): string {
  const map: Record<string, string> = {
    downloading: 'text-accent bg-accent/10',
    completed: 'text-success bg-success/10',
    paused: 'text-text-secondary bg-white/5',
    failed: 'text-error bg-error/10',
    pending: 'text-warning bg-warning/10',
    cancelled: 'text-text-tertiary bg-white/5',
  }
  return map[status] || 'text-text-secondary bg-white/5'
}
