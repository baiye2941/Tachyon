import { createSignal } from 'solid-js'

const [selectedIds, setSelectedIds] = createSignal<Set<string>>(new Set())

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
}

export function selectAll(ids: string[]) {
  setSelectedIds(new Set<string>(ids))
}

export function deselectAll() {
  setSelectedIds(new Set<string>())
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

export const $selectedIds = {
  get: selectedIds,
}
