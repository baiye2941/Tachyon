import { Show, onMount, onCleanup } from 'solid-js'
import { selectedCount, hasSelection, deselectAll, selectAll } from '../stores/selection'
import { $tasks } from '../stores/downloads'
import { Icon } from '../utils/icons'
import { btnIcon } from '../utils/styles'

interface BatchToolbarProps {
  onPauseAll: () => void
  onResumeAll: () => void
  onDeleteAll: () => void
}

export default function BatchToolbar(props: BatchToolbarProps) {
  const count = () => selectedCount()
  const visible = () => hasSelection()
  const taskIds = () => $tasks.get().map(t => t.id)

  onMount(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Delete' && hasSelection()) {
        e.preventDefault()
        props.onDeleteAll()
      }
      if (e.key === 'a' && e.ctrlKey) {
        e.preventDefault()
        selectAll(taskIds())
      }
    }
    document.addEventListener('keydown', handler)
    onCleanup(() => document.removeEventListener('keydown', handler))
  })

  return (
    <Show when={visible()}>
      <div
        role="toolbar"
        aria-label="批量操作"
        class="fixed bottom-3 left-1/2 -translate-x-1/2 z-50 flex items-center gap-1.5 px-3 py-1.5 glass-panel rounded-lg shadow-lg shadow-black/40 backdrop-blur-sm animate-slide-up"
      >
        <span class="text-[10px] text-text-secondary mr-1 font-mono">
          已选 {count()} 项
        </span>

        <button
          class={`flex items-center gap-1 px-2 py-1 text-[10px] rounded ${btnIcon} w-auto h-auto text-text-secondary hover:text-text-primary`}
          onClick={() => props.onPauseAll()}
          aria-label="批量暂停"
        >
          <Icon name="pause" class="w-4 h-4" /> 暂停
        </button>

        <button
          class={`flex items-center gap-1 px-2 py-1 text-[10px] rounded ${btnIcon} w-auto h-auto text-text-secondary hover:text-text-primary`}
          onClick={() => props.onResumeAll()}
          aria-label="批量恢复"
        >
          <Icon name="play" class="w-4 h-4" /> 恢复
        </button>

        <div class="w-px h-3 bg-border-strong mx-0.5" />

        <button
          class={`flex items-center gap-1 px-2 py-1 text-[10px] rounded ${btnIcon} w-auto h-auto text-error hover:text-error`}
          onClick={() => props.onDeleteAll()}
          aria-label="批量删除"
        >
          <Icon name="trash" class="w-4 h-4" /> 删除
        </button>

        <button
          class={`flex items-center gap-1 px-2 py-1 text-[10px] rounded ${btnIcon} w-auto h-auto`}
          onClick={deselectAll}
          aria-label="清空选择"
        >
          清空
        </button>
      </div>
    </Show>
  )
}
