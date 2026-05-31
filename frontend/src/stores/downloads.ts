import { createSignal, batch } from 'solid-js'
import { createStore, reconcile } from 'solid-js/store'
import type { TaskInfo, DownloadStatus, ProgressPayload } from '../types'

const [tasks, setTasksRaw] = createStore<TaskInfo[]>([])
const [selectedId, setSelectedId] = createSignal<string | null>(null)

const ACTIVE_STATUSES: DownloadStatus[] = ['downloading', 'paused', 'pending']
const COMPLETED_STATUSES: DownloadStatus[] = ['completed', 'failed', 'cancelled']

export function setTasks(newTasks: TaskInfo[]) {
  batch(() => setTasksRaw(reconcile(newTasks, { key: 'id' })))
}

export { setSelectedId }

export const $tasks = {
  get: () => tasks,
  set: setTasks,
}

export const $selectedId = {
  get: selectedId,
  set: setSelectedId,
}

export const $activeTasks = {
  get: () => tasks.filter(t => ACTIVE_STATUSES.includes(t.status)),
}

export const $completedTasks = {
  get: () => tasks.filter(t => COMPLETED_STATUSES.includes(t.status)),
}

export const $totalSpeed = {
  get: () => tasks.filter(t => ACTIVE_STATUSES.includes(t.status)).reduce((sum, t) => sum + (t.speed || 0), 0),
}

export function updateProgress(payload: Record<string, ProgressPayload>) {
  batch(() => {
    setTasksRaw(reconcile(
      tasks.map(t => {
        const p = payload[t.id]
        if (!p) return t
        return {
          ...t,
          downloaded: p.downloaded ?? t.downloaded,
          speed: p.speed ?? t.speed,
          status: (p.status as DownloadStatus) ?? t.status,
          progress: p.progress ?? t.progress,
          fragmentsDone: p.fragmentsDone ?? t.fragmentsDone,
        }
      }),
      { key: 'id' }
    ))
  })
}