import { api } from '../api/invoke'
import { $tasks, refreshTaskList } from './downloads'
import { $selectedIds, deselectAll } from './selection'
import { addToast } from './toast'

async function confirm(message: string): Promise<boolean> {
  try {
    const { confirm } = await import('@tauri-apps/plugin-dialog')
    return await confirm(message, { title: '确认操作', kind: 'warning' })
  } catch {
    return window.confirm(message)
  }
}

export async function pauseSelected(): Promise<void> {
  const ids = Array.from($selectedIds.get())
  if (ids.length === 0) return

  await Promise.allSettled(ids.map(id => api.pauseTask(id)))
  deselectAll()
  await refreshTaskList()
}

export async function resumeSelected(): Promise<void> {
  const ids = Array.from($selectedIds.get())
  if (ids.length === 0) return

  await Promise.allSettled(ids.map(id => api.resumeTask(id)))
  deselectAll()
  await refreshTaskList()
}

export async function deleteSelected(): Promise<void> {
  const ids = Array.from($selectedIds.get())
  if (ids.length === 0) return

  const confirmed = await confirm(`确定要删除选中的 ${ids.length} 个任务吗？`)
  if (!confirmed) return

  await Promise.allSettled(ids.map(id => api.deleteTask(id)))
  deselectAll()
  await refreshTaskList()
}

export async function pauseAll(): Promise<void> {
  const ids = $tasks.get()
    .filter(t => t.status === 'downloading' || t.status === 'connecting' || t.status === 'resuming')
    .map(t => t.id)

  if (ids.length === 0) {
    addToast('没有可暂停的任务', 'info')
    return
  }

  await Promise.allSettled(ids.map(id => api.pauseTask(id)))
  await refreshTaskList()
}

export async function resumeAll(): Promise<void> {
  const ids = $tasks.get()
    .filter(t => t.status === 'paused')
    .map(t => t.id)

  if (ids.length === 0) {
    addToast('没有可恢复的任务', 'info')
    return
  }

  await Promise.allSettled(ids.map(id => api.resumeTask(id)))
  await refreshTaskList()
}
