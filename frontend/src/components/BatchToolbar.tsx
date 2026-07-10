import { Show, onMount, onCleanup } from 'solid-js'
import { selectedCount, hasSelection, deselectAll, selectAll } from '../stores/selection'
import { $taskFilter } from '../stores/taskFilter'
import { Icon } from '../utils/icons'
import Button from '../shared/ui/Button'
import { tr } from '../i18n'

interface BatchToolbarProps {
  onPauseAll: () => void
  onResumeAll: () => void
  onDeleteAll: () => void
}

export default function BatchToolbar(props: BatchToolbarProps) {
  const count = () => selectedCount()
  const visible = () => hasSelection()
  const taskIds = () => $taskFilter.filteredTasks().map(t => t.id)

  onMount(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Delete' && hasSelection()) {
        e.preventDefault()
        props.onDeleteAll()
      }
      // Ctrl+A: 焦点在输入框/textarea 内时不拦截,保留浏览器原生全选
      if (e.key.toLowerCase() === 'a' && (e.ctrlKey || e.metaKey)) {
        const target = e.target as HTMLElement
        const tag = target.tagName
        if (tag === 'INPUT' || tag === 'TEXTAREA' || target.isContentEditable) {
          return
        }
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
        aria-label={tr("batch.aria")}
        class="fixed bottom-3 left-1/2 -translate-x-1/2 z-[var(--z-dropdown)] flex items-center gap-1 px-3 py-2 rounded-lg panel-surface"
        style={{
          'box-shadow': 'var(--shadow-lg)',
          animation: 'card-fade-in 200ms var(--ease-emphasized) forwards',
        }}
      >
        <span
          class="mono flex items-center justify-center"
          style={{
            "min-width": "20px",
            height: "20px",
            padding: "0 6px",
            "border-radius": "5px",
            "font-size": "12px",
            "font-weight": 600,
            color: "var(--color-accent-foreground)",
            background: "var(--color-accent-primary)",
            "margin-right": "4px",
          }}
        >
          {count()}
        </span>
        <span
          style={{
            "font-size": "13px",
            color: "var(--color-text-secondary)",
            "margin-right": "8px",
          }}
        >
          {tr('batch.selectedCount', { count: count() })}
        </span>

        <Button
          variant="ghost"
          size="sm"
          onClick={() => props.onPauseAll()}
          aria-label={tr("batch.aria.pause")}
        >
          <Icon name="pause" class="w-4 h-4" />
          <span>{tr("common.pause")}</span>
        </Button>

        <Button
          variant="ghost"
          size="sm"
          onClick={() => props.onResumeAll()}
          aria-label={tr("batch.aria.resume")}
        >
          <Icon name="play" class="w-4 h-4" />
          <span>{tr("common.resume")}</span>
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
          aria-label={tr("batch.aria.delete")}
        >
          <Icon name="trash" class="w-4 h-4" />
          <span>{tr("common.delete")}</span>
        </Button>

        <Button
          variant="ghost"
          size="sm"
          onClick={deselectAll}
          aria-label={tr("batch.aria.clear")}
        >
          {tr("common.clearSelection")}
        </Button>
      </div>
    </Show>
  )
}
