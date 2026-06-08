import { describe, it, expect, beforeEach } from 'vitest'
import { createRoot, batch } from 'solid-js'
import { createStore, reconcile } from 'solid-js/store'
import type { TaskInfo, ProgressEvent } from '../../types'

const makeTask = (id: string, overrides: Partial<TaskInfo> = {}): TaskInfo => ({
  id,
  url: `https://example.com/${id}.bin`,
  fileName: `${id}.bin`,
  fileSize: 1048576,
  downloaded: 0,
  speed: 0,
  status: 'pending',
  progress: 0,
  fragmentsTotal: 4,
  fragmentsDone: 0,
  createdAt: '2026-05-30T00:00:00Z',
  ...overrides,
})

function applyProgress(
  tasks: TaskInfo[],
  payload: ProgressEvent
): TaskInfo[] {
  return tasks.map(t => {
    const p = payload[t.id]
    if (!p) return t
    return {
      ...t,
      downloaded: p.downloaded ?? t.downloaded,
      speed: p.speed ?? t.speed,
      status: (p.status as TaskInfo['status']) ?? t.status,
      progress: p.progress ?? t.progress,
      fragmentsDone: p.fragmentsDone ?? t.fragmentsDone,
    }
  })
}

describe('ChannelStore 高频更新', () => {
  let tasks: TaskInfo[]
  let setTasksRaw: ReturnType<typeof createStore<TaskInfo[]>>[1]

  beforeEach(() => {
    createRoot((_dispose) => {
      const [store, setter] = createStore<TaskInfo[]>([])
      // eslint-disable-next-line solid/reactivity -- 测试 harness 需要保留 store 代理供后续断言读取
      tasks = store
      setTasksRaw = setter
    })
  })

  it('单次 reconcile 后 Store 状态一致', () => {
    const initial = [makeTask('a'), makeTask('b')]
    setTasksRaw(reconcile(initial, { key: 'id' }))

    const p1: ProgressEvent = {
      a: { id: 'a', progress: 0.3, downloaded: 314572, speed: 5000000, status: 'downloading', fragmentsDone: 1 },
    }
    batch(() => {
      setTasksRaw(reconcile(applyProgress([...tasks], p1), { key: 'id' }))
    })

    expect(tasks.find(t => t.id === 'a')!.progress).toBe(0.3)
    expect(tasks.find(t => t.id === 'b')!.progress).toBe(0)
    expect(tasks.find(t => t.id === 'a')!.speed).toBe(5000000)
  })

  it('高频 (>100Hz) 连续 batch 不丢失最终状态', () => {
    const initial = [makeTask('x', { fragmentsTotal: 100 })]
    setTasksRaw(reconcile(initial, { key: 'id' }))

    for (let i = 0; i < 200; i++) {
      batch(() => {
        const payload: ProgressEvent = {
          x: {
            id: 'x',
            progress: i / 200,
            downloaded: Math.floor(1048576 * (i / 200)),
            speed: 10000000 + i * 1000,
            status: 'downloading',
            fragmentsDone: Math.floor(100 * (i / 200)),
          },
        }
        setTasksRaw(reconcile(applyProgress([...tasks], payload), { key: 'id' }))
      })
    }

    const task = tasks.find(t => t.id === 'x')!
    expect(task.fragmentsDone).toBe(99)
    expect(task.status).toBe('downloading')
  })

  it('reconcile 仅更新变更节点（key 去重）', () => {
    const initial = [makeTask('m'), makeTask('n'), makeTask('o')]
    setTasksRaw(reconcile(initial, { key: 'id' }))

    const payload: ProgressEvent = {
      n: { id: 'n', progress: 0.8, downloaded: 838860, speed: 12000000, status: 'downloading', fragmentsDone: 3 },
    }

    batch(() => {
      setTasksRaw(reconcile(applyProgress([...tasks], payload), { key: 'id' }))
    })

    expect(tasks.find(t => t.id === 'm')!.progress).toBe(0)
    expect(tasks.find(t => t.id === 'n')!.progress).toBe(0.8)
    expect(tasks.find(t => t.id === 'o')!.progress).toBe(0)
  })

  it('空 payload 不改变 Store', () => {
    const initial = [makeTask('p', { progress: 0.42 })]
    setTasksRaw(reconcile(initial, { key: 'id' }))

    batch(() => {
      setTasksRaw(reconcile(applyProgress([...tasks], {}), { key: 'id' }))
    })

    expect(tasks.find(t => t.id === 'p')!.progress).toBe(0.42)
  })

  it('新任务追加时 reconcile 正确扩展', () => {
    const initial = [makeTask('q')]
    setTasksRaw(reconcile(initial, { key: 'id' }))

    const newTask = makeTask('r', { status: 'pending', progress: 0 })
    batch(() => {
      setTasksRaw(reconcile([...applyProgress([...tasks], {}), newTask], { key: 'id' }))
    })

    expect(tasks.length).toBe(2)
    expect(tasks.find(t => t.id === 'r')!.status).toBe('pending')
  })
})
