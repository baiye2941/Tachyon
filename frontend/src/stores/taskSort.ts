/**
 * 任务列表排序状态(Iteration 07,DI-3)。
 *
 * 排序 key/dir 状态 + 纯比较器 sortTasks。与 taskFilter 解耦:
 * 排序作用于「已筛选的列表」末端(调用方组合)。
 *
 * 状态机:
 * - toggle(key):同列切换 asc↔desc;异列重置为 desc(默认降序,符合
 *   「找最慢/最大」直觉);首次点击从 desc 开始。
 * - clear():回到无排序(原序)。
 */

import { createSignal, type Accessor } from 'solid-js'
import type { TaskInfo, DownloadStatus } from '../types'
import type { SortKey, SortDir } from '../components/taskColumns'

export interface SortState {
  key: SortKey | null
  dir: SortDir
}

const [sortState, setSortState] = createSignal<SortState>({ key: null, dir: 'desc' })

export const $taskSort = {
  get state(): Accessor<SortState> {
    return sortState
  },
}

/** 切换排序:同列反转方向,异列重置为 desc */
export function toggleSort(key: SortKey): void {
  setSortState((prev) => {
    if (prev.key === key) {
      return { key, dir: prev.dir === 'asc' ? 'desc' : 'asc' }
    }
    return { key, dir: 'desc' }
  })
}

/** 清除排序(回到原序) */
export function clearSort(): void {
  setSortState({ key: null, dir: 'desc' })
}

/**
 * 状态排序权重:用于按 status 排序。
 * 活跃状态(下载/连接/恢复)权重最高——desc 时活跃任务排前(符合
 * 「找正在进行的任务」直觉),asc 时已完成/失败排前。
 * dir=desc 时大值在前,故活跃=大值。
 */
const STATUS_RANK: Record<DownloadStatus, number> = {
  downloading: 8,
  connecting: 7,
  resuming: 6,
  verifying: 5,
  pending: 4,
  paused: 3,
  completed: 2,
  cancelled: 1,
  failed: 0,
}

type Comparator = (a: TaskInfo, b: TaskInfo) => number

const COMPARATORS: Record<Exclude<SortKey, "name">, Comparator> = {
  progress: (a, b) => a.progress - b.progress,
  speed: (a, b) => a.speed - b.speed,
  status: (a, b) => STATUS_RANK[a.status] - STATUS_RANK[b.status],
  size: (a, b) => (a.fileSize ?? 0) - (b.fileSize ?? 0),
  remaining: (a, b) => {
    const ra = a.fileSize ? a.fileSize - a.downloaded : Number.POSITIVE_INFINITY;
    const rb = b.fileSize ? b.fileSize - b.downloaded : Number.POSITIVE_INFINITY;
    return ra - rb;
  },
  downloaded: (a, b) => a.downloaded - b.downloaded,
  fragments: (a, b) => {
    const pa = a.fragmentsTotal > 0 ? a.fragmentsDone / a.fragmentsTotal : 0;
    const pb = b.fragmentsTotal > 0 ? b.fragmentsDone / b.fragmentsTotal : 0;
    return pa - pb;
  },
  threads: (a, b) => (a.activeConcurrency ?? 0) - (b.activeConcurrency ?? 0),
  createdAt: (a, b) =>
    new Date(a.createdAt).getTime() - new Date(b.createdAt).getTime(),
};

/**
 * 按排序状态对任务列表排序(纯函数,不修改原数组)。
 *
 * key 为 null 时返回原序(浅拷贝保持不可变)。dir=asc 升序,desc 降序。
 * 同序值时按 fileName 稳定排序(避免等值抖动)。
 */
export function sortTasks(tasks: TaskInfo[], state: SortState): TaskInfo[] {
  if (state.key === null || state.key === 'name') return [...tasks]
  const baseCmp = COMPARATORS[state.key]
  const dirMul = state.dir === 'asc' ? 1 : -1
  return [...tasks].sort((a, b) => {
    const primary = baseCmp(a, b) * dirMul
    if (primary !== 0) return primary
    // 稳定:同序值按文件名
    return a.fileName.localeCompare(b.fileName)
  })
}

/**
 * 分组视图下组内排序。
 *
 * - 若排序 key 为 progress/speed/status,调用现有 sortTasks 保持全局排序语义。
 * - 若 key 为 null 或 name(name 不参与排序),按 createdAt 降序 → fileName 升序稳定排序。
 *
 * 这样每组内部的新鲜任务排在前面,同时避免等值抖动。
 */
export function sortGroupTasks(tasks: TaskInfo[], state: SortState): TaskInfo[] {
  if (state.key === 'progress' || state.key === 'speed' || state.key === 'status') {
    return sortTasks(tasks, state)
  }
  return [...tasks].sort((a, b) => {
    const ta = new Date(a.createdAt).getTime()
    const tb = new Date(b.createdAt).getTime()
    if (ta !== tb) return tb - ta
    return a.fileName.localeCompare(b.fileName)
  })
}
