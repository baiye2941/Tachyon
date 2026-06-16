import { createSignal, batch } from 'solid-js'
import { createStore, reconcile } from 'solid-js/store'
import type { TaskInfo, DownloadStatus, ProgressPayload, DownloadFilter } from '../types'
import { api } from '../api/invoke'
import { addToast } from './toast'
import { addHistoryRecord } from './history'
import { createRootMemo } from '../utils/reactive'

// ── 高频进度数据(hot 层,250ms 级更新) ─────────────────────────
//
// 将进度/速度等高频变化字段拆分到独立 signal,避免每次 progress tick
// 触发 tasks store 的 reconcile,从而减少低频字段(文件名/URL/路径)
// 依赖组件的无谓重渲染。hot 层以 task id 为 key,只包含每帧真正变化的数值。

export interface HotProgress {
  downloaded: number
  speed: number
  progress: number
  fragmentsDone: number
}

const [hotProgress, setHotProgress] = createSignal<Map<string, HotProgress>>(new Map())

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
    taskIndexMap.set(tasks[i]!.id, i)
  }
}

export function setTasks(newTasks: TaskInfo[]) {
  batch(() => {
    setTasksRaw(reconcile(newTasks, { key: 'id' }))
    rebuildIndexMap()
    // 同步初始化 hot 层:从全量任务列表提取高频字段
    const hotMap = new Map<string, HotProgress>()
    for (const t of newTasks) {
      hotMap.set(t.id, {
        downloaded: t.downloaded,
        speed: t.speed,
        progress: t.progress,
        fragmentsDone: t.fragmentsDone,
      })
    }
    setHotProgress(hotMap)
  })
}

export { setSelectedId, setCurrentFilter }

export const $hotProgress = {
  get: hotProgress,
}

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

const filteredTasks = createRootMemo(() => {
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
const filterCounts = createRootMemo(() => {
  let downloading = 0
  let completed = 0
  let incomplete = 0
  for (let i = 0; i < tasks.length; i++) {
    const s = tasks[i]!.status
    if (DOWNLOADING_SET.has(s)) downloading++
    if (COMPLETED_SET.has(s)) completed++
    if (INCOMPLETE_SET.has(s)) incomplete++
  }
  return { all: tasks.length, downloading, completed, incomplete }
})

export const $filterCounts = {
  get: filterCounts,
}

const selectedTask = createRootMemo(() => {
  const id = selectedId()
  if (!id) return null
  return tasks.find(t => t.id === id) ?? null
})

export const $selectedTask = {
  get: selectedTask,
}

// totalSpeed 和 activeCount 从 hot 层读取,避免高频 progress tick
// 触发 tasks store 的 reconcile 导致低频字段依赖组件无谓重渲染
const speedStats = createRootMemo(() => {
  let speed = 0
  let count = 0
  const hot = hotProgress()
  for (let i = 0; i < tasks.length; i++) {
    if (DOWNLOADING_SET.has(tasks[i]!.status)) {
      const hp = hot.get(tasks[i]!.id)
      speed += hp?.speed ?? (tasks[i]!.speed || 0)
      count++
    }
  }
  return { speed, count }
})

const totalSpeed = createRootMemo(() => speedStats().speed)
const activeCount = createRootMemo(() => speedStats().count)

export const $totalSpeed = {
  get: totalSpeed,
}

export const $activeCount = {
  get: activeCount,
}

export function updateProgress(payload: Record<string, ProgressPayload>) {
  const TERMINAL_STATUSES = new Set<DownloadStatus>(['completed', 'failed', 'cancelled'])

  batch(() => {
    // hot 层增量更新:收集所有变化的 high-frequency 字段
    const hotUpdates = new Map<string, HotProgress>()

    for (const [id, p] of Object.entries(payload)) {
      const idx = taskIndexMap.get(id)    // O(1) 查找
      if (idx === undefined) continue

      const task = tasks[idx]!
      const oldStatus = task.status
      const newStatus = VALID_STATUSES.has(p.status) ? (p.status as DownloadStatus) : oldStatus

      const newDownloaded = p.downloaded ?? task.downloaded
      const newSpeed = p.speed ?? task.speed
      const newProgress = p.progress ?? task.progress
      const newFragmentsDone = p.fragmentsDone ?? task.fragmentsDone

      // hot 层:高频字段变化时更新 hotProgress signal
      const hotChanged =
        newDownloaded !== task.downloaded ||
        newSpeed !== task.speed ||
        newProgress !== task.progress ||
        newFragmentsDone !== task.fragmentsDone

      if (hotChanged) {
        hotUpdates.set(id, {
          downloaded: newDownloaded,
          speed: newSpeed,
          progress: newProgress,
          fragmentsDone: newFragmentsDone,
        })
      }

      // cold 层:status 变化时才更新 tasks store(低频)
      // 同时 hot 层变化时也需同步 tasks store,保持数据一致性
      const hasChanged = hotChanged || newStatus !== oldStatus

      // 只有至少一个字段真正变化时才更新 store，避免无意义 reconcile
      if (hasChanged) {
        setTasksRaw(idx, {
          downloaded: newDownloaded,
          speed: newSpeed,
          status: newStatus,
          progress: newProgress,
          fragmentsDone: newFragmentsDone,
        })
      }

      // 状态转 terminal：只在 status 真正变化到 terminal 时触发
      if (oldStatus !== newStatus && !TERMINAL_STATUSES.has(oldStatus) && TERMINAL_STATUSES.has(newStatus)) {
        const updatedTask = tasks[idx]!
        const duration = updatedTask.createdAt ? Date.now() - new Date(updatedTask.createdAt).getTime() : 0
        const avgSpeed = duration > 0 ? (updatedTask.downloaded || 0) / (duration / 1000) : 0

        addHistoryRecord({
          url: updatedTask.url,
          fileName: updatedTask.fileName,
          fileSize: updatedTask.fileSize || 0,
          status: newStatus as 'completed' | 'failed' | 'cancelled',
          duration: Math.floor(duration / 1000), // 秒
          avgSpeed,
        })
      }
    }

    // 批量更新 hot 层 signal
    if (hotUpdates.size > 0) {
      setHotProgress(prev => {
        const next = new Map(prev)
        for (const [id, hp] of hotUpdates) {
          next.set(id, hp)
        }
        return next
      })
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
