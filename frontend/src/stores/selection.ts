import { createSignal } from 'solid-js'

const [selectedIds, setSelectedIds] = createSignal<Set<string>>(new Set())
const [lastSelectedAnchorId, setLastSelectedAnchorId] = createSignal<string | null>(null)

export function toggleSelection(id: string) {
  setSelectedIds(prev => {
    const next = new Set(prev)
    if (next.has(id)) {
      next.delete(id)
    } else {
      next.add(id)
    }
    return next
  })
  setLastSelectedAnchorId(id)
}

/** Shift + 点击连选:以 anchor 为起点,选中 anchor 到当前项之间的全部 */
export function selectRange(anchorId: string, endId: string, allIds: string[]) {
  const anchorIdx = allIds.indexOf(anchorId)
  const endIdx = allIds.indexOf(endId)
  if (anchorIdx === -1 || endIdx === -1) return
  const start = Math.min(anchorIdx, endIdx)
  const end = Math.max(anchorIdx, endIdx)
  const rangeIds = allIds.slice(start, end + 1)
  setSelectedIds(prev => {
    const next = new Set(prev)
    for (const id of rangeIds) {
      next.add(id)
    }
    return next
  })
  // anchor 保持不动,方便连续多次 Shift 点击扩展选择
}

export function selectAll(ids: string[]) {
  setSelectedIds(new Set<string>(ids))
  setLastSelectedAnchorId(ids[ids.length - 1] ?? null)
}

export function deselectAll() {
  setSelectedIds(new Set<string>())
  setLastSelectedAnchorId(null)
}

/**
 * 将当前选择集与可见任务 ID 取交集。
 * 当过滤条件变化导致某些已选任务被隐藏时,调用此函数可保持
 * "已选 N 项" 与批量操作范围始终与当前可见列表一致。
 */
export function intersectSelection(visibleIds: Set<string>): void {
  setSelectedIds((prev) => {
    let changed = false
    for (const id of prev) {
      if (!visibleIds.has(id)) {
        changed = true
        break
      }
    }
    if (!changed) return prev

    const next = new Set<string>()
    for (const id of prev) {
      if (visibleIds.has(id)) next.add(id)
    }
    return next
  })
}

export function isSelected(id: string): boolean {
  return selectedIds().has(id)
}

export function selectedCount(): number {
  return selectedIds().size
}

export function hasSelection(): boolean {
  return selectedIds().size > 0
}

export function getLastSelectedAnchorId(): string | null {
  return lastSelectedAnchorId()
}

export { setLastSelectedAnchorId }

export const $selectedIds = {
  get: selectedIds,
}
