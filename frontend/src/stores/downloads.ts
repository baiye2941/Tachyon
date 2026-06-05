import { createSignal, batch, createMemo } from 'solid-js'
import { createStore, reconcile } from 'solid-js/store'
import type { TaskInfo, DownloadStatus, ProgressPayload, DownloadFilter } from '../types'
import { api } from '../api/invoke'
import { addToast } from './toast'

const VALID_STATUSES = new Set<string>(['pending', 'connecting', 'downloading', 'paused', 'resuming', 'verifying', 'completed', 'failed', 'cancelled'])

const DOWNLOADING_STATUSES: DownloadStatus[] = ['connecting', 'downloading', 'resuming', 'verifying']
const INCOMPLETE_STATUSES: DownloadStatus[] = ['pending', 'connecting', 'downloading', 'paused', 'resuming', 'verifying']
const COMPLETED_STATUSES: DownloadStatus[] = ['completed']

// 预构建 Set，将 .includes() 从 O(k) 降至 O(1)
const DOWNLOADING_SET = new Set<DownloadStatus>(DOWNLOADING_STATUSES)
const INCOMPLETE_SET = new Set<DownloadStatus>(INCOMPLETE_STATUSES)
const COMPLETED_SET = new Set<DownloadStatus>(COMPLETED_STATUSES)

const [tasks, setTasksRaw] = createStore<TaskInfo[]>([])
const [selectedId, setSelectedId] = createSignal<string | null>(null)
const [currentFilter, setCurrentFilter] = createSignal<DownloadFilter>('all')

// 任务 ID → 数组索引映射，updateProgress 从 O(m*n) 降至 O(m)
let taskIndexMap = new Map<string, number>()

function rebuildIndexMap() {
  taskIndexMap = new Map<string, number>()
  for (let i = 0; i < tasks.length; i++) {
    taskIndexMap.set(tasks[i].id, i)
  }
}

export function setTasks(newTasks: TaskInfo[]) {
  batch(() => {
    setTasksRaw(reconcile(newTasks, { key: 'id' }))
    rebuildIndexMap()
  })
}

export { setSelectedId, setCurrentFilter }

export const $tasks = {
  get: () => tasks,
  set: setTasks,
}

export const $selectedId = {
  get: selectedId,
  set: setSelectedId,
}

export const $currentFilter = {
  get: currentFilter,
  set: setCurrentFilter,
}

const filteredTasks = createMemo(() => {
  const filter = currentFilter()
  switch (filter) {
    case 'downloading':
      return tasks.filter(t => DOWNLOADING_SET.has(t.status))
    case 'completed':
      return tasks.filter(t => COMPLETED_SET.has(t.status))
    case 'incomplete':
      return tasks.filter(t => INCOMPLETE_SET.has(t.status))
    default:
      return tasks
  }
})

export const $filteredTasks = {
  get: filteredTasks,
}

// 单次遍历统计四个计数器，替代原来 3 次独立 filter
const filterCounts = createMemo(() => {
  let downloading = 0
  let completed = 0
  let incomplete = 0
  for (let i = 0; i < tasks.length; i++) {
    const s = tasks[i].status
    if (DOWNLOADING_SET.has(s)) downloading++
    if (COMPLETED_SET.has(s)) completed++
    if (INCOMPLETE_SET.has(s)) incomplete++
  }
  return { all: tasks.length, downloading, completed, incomplete }
})

export const $filterCounts = {
  get: filterCounts,
}

const selectedTask = createMemo(() => {
  const id = selectedId()
  if (!id) return null
  return tasks.find(t => t.id === id) ?? null
})

export const $selectedTask = {
  get: selectedTask,
}

// totalSpeed 和 activeCount 共享一次遍历
const speedStats = createMemo(() => {
  let speed = 0
  let count = 0
  for (let i = 0; i < tasks.length; i++) {
    if (DOWNLOADING_SET.has(tasks[i].status)) {
      speed += tasks[i].speed || 0
      count++
    }
  }
  return { speed, count }
})

const totalSpeed = createMemo(() => speedStats().speed)
const activeCount = createMemo(() => speedStats().count)

export const $totalSpeed = {
  get: totalSpeed,
}

export const $activeCount = {
  get: activeCount,
}

export function updateProgress(payload: Record<string, ProgressPayload>) {
  batch(() => {
    for (const [id, p] of Object.entries(payload)) {
      const idx = taskIndexMap.get(id)    // O(1) 查找
      if (idx !== undefined) {
        setTasksRaw(idx, {
          downloaded: p.downloaded ?? tasks[idx].downloaded,
          speed: p.speed ?? tasks[idx].speed,
          status: VALID_STATUSES.has(p.status) ? (p.status as DownloadStatus) : tasks[idx].status,
          progress: p.progress ?? tasks[idx].progress,
          fragmentsDone: p.fragmentsDone ?? tasks[idx].fragmentsDone,
        })
      }
    }
  })
}

export async function refreshTaskList() {
  try {
    const tasks = await api.getTaskList()
    setTasks(tasks)
  } catch (e) {
    addToast('刷新任务列表失败: ' + String(e), 'error')
  }
}
