import { Show, onMount, onCleanup } from 'solid-js'
import { selectedCount, hasSelection, deselectAll, selectAll } from '../stores/selection'
import { $tasks } from '../stores/downloads'
import { Icon } from '../utils/icons'
import Button from '../shared/ui/Button'

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
        class="fixed bottom-3 left-1/2 -translate-x-1/2 z-50 flex items-center gap-1 px-3 py-2 rounded-lg panel-surface"
        style={{
          'box-shadow': 'var(--shadow-lg)',
          animation: 'card-fade-in 200ms var(--ease-emphasized) forwards',
        }}
      >
        <span
          class="mr-1 mono"
          style={{
            'font-size': '11px',
            color: 'var(--color-text-secondary)',
          }}
        >
          已选 {count()} 项
        </span>

        <Button
          variant="ghost"
          size="sm"
          onClick={() => props.onPauseAll()}
          aria-label="批量暂停"
        >
          <Icon name="pause" class="w-4 h-4" />
          <span>暂停</span>
        </Button>

        <Button
          variant="ghost"
          size="sm"
          onClick={() => props.onResumeAll()}
          aria-label="批量恢复"
        >
          <Icon name="play" class="w-4 h-4" />
          <span>恢复</span>
        </Button>

        <div
          class="mx-1"
          style={{
            width: '1px',
            height: '14px',
            background: 'var(--color-border-default)',
          }}
        />

        <Button
          variant="danger"
          size="sm"
          onClick={() => props.onDeleteAll()}
          aria-label="批量删除"
        >
          <Icon name="trash" class="w-4 h-4" />
          <span>删除</span>
        </Button>

        <Button
          variant="ghost"
          size="sm"
          onClick={deselectAll}
          aria-label="清空选择"
        >
          清空
        </Button>
      </div>
    </Show>
  )
}
