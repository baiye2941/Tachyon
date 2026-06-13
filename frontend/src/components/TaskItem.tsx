import { createMemo, For, Show } from 'solid-js'
import type { TaskInfo, ListDensity } from '../types'
import { CheckboxIcon } from './icons'
import { formatSize, formatSpeed, getFileType, getStatusColor, getStatusLabel } from '../utils/format'

interface TaskItemProps {
  task: TaskInfo
  isSelected: boolean
  isMultiSelected: boolean
  isMultiSelectMode: boolean
  onClick: () => void
  onContextMenu?: (e: MouseEvent) => void
  density: ListDensity
  searchQuery?: string
  staggerIndex?: number
}

function HighlightedText(props: { text: string; query: string }) {
  const parts = createMemo(() => {
    const text = props.text
    const query = props.query.trim()

    if (!query) return [{ text, highlight: false }]

    const lowerText = text.toLowerCase()
    const lowerQuery = query.toLowerCase()
    const result: { text: string; highlight: boolean }[] = []

    let i = 0
    while (i < text.length) {
      const idx = lowerText.indexOf(lowerQuery, i)
      if (idx === -1) {
        result.push({ text: text.slice(i), highlight: false })
        break
      }
      if (idx > i) {
        result.push({ text: text.slice(i, idx), highlight: false })
      }
      result.push({ text: text.slice(idx, idx + query.length), highlight: true })
      i = idx + query.length
    }

    return result
  })

  return (
    <>
      <For each={parts()}>
        {(part) => part.highlight ? (
          <span
            style={{
              background: 'rgba(0, 212, 170, 0.2)',
              color: '#00D4AA',
              'border-radius': '2px',
              padding: '0 2px',
            }}
          >
            {part.text}
          </span>
        ) : (
          <>{part.text}</>
        )}
      </For>
    </>
  )
}

export default function TaskItem(props: TaskItemProps) {
  const fileInfo = createMemo(() => getFileType(props.task.fileName))
  const isCompact = () => props.density === 'compact'

  const handleKeyDown = (e: KeyboardEvent) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault()
      props.onClick()
    }
  }

  const ariaLabel = () => {
    const progress = (props.task.progress * 100).toFixed(1)
    const status = getStatusLabel(props.task.status)
    return `任务：${props.task.fileName}，进度 ${progress}%，状态 ${status}`
  }

  return (
    <div
      role="button"
      tabindex="0"
      aria-label={ariaLabel()}
      class="cursor-pointer transition-all duration-150 hover-lift-sm task-item-enter focus:outline-none focus-visible:ring-2 focus-visible:ring-[#00D4AA] focus-visible:ring-offset-2 focus-visible:ring-offset-[#0A0A12]"
      style={{
        padding: isCompact() ? '8px 16px' : '12px 16px',
        background: props.isMultiSelected
          ? 'rgba(0, 212, 170, 0.08)'
          : props.isSelected
            ? 'rgba(0, 212, 170, 0.04)'
            : 'transparent',
        'border-left': props.isMultiSelected ? '2px solid #00D4AA' : '2px solid transparent',
        '--stagger-index': props.staggerIndex ?? 0,
      }}
      onClick={() => props.onClick()}
      onKeyDown={handleKeyDown}
      onContextMenu={(e) => props.onContextMenu?.(e)}
    >
      <div class="flex items-center gap-3">
        <Show when={props.isMultiSelectMode}>
          <div
            class="flex items-center justify-center flex-shrink-0"
            role="checkbox"
            aria-checked={props.isMultiSelected}
            style={{
              width: '20px',
              height: '20px',
              color: props.isMultiSelected ? '#00D4AA' : '#6B7280',
            }}
          >
            <CheckboxIcon checked={props.isMultiSelected} />
          </div>
        </Show>

        <div
          class="flex items-center justify-center flex-shrink-0"
          style={{
            width: isCompact() ? '32px' : '40px',
            height: isCompact() ? '32px' : '40px',
            color: fileInfo().color,
          }}
        >
          {(() => {
            const Icon = fileInfo().icon
            return <Icon />
          })()}
        </div>

        <div class="flex-1 min-w-0">
          <div class="flex items-center justify-between min-w-0">
            <div class="flex-1 min-w-0">
              <div
                class="truncate"
                style={{
                  'font-size': '14px',
                  'font-weight': 500,
                  color: '#F0F0F5',
                }}
              >
                <HighlightedText text={props.task.fileName} query={props.searchQuery || ''} />
              </div>
              <Show when={!isCompact()}>
                <div
                  class="truncate"
                  style={{
                    'font-size': '12px',
                    color: '#A0A0B0',
                    'margin-top': '2px',
                  }}
                >
                  {props.task.fileSize ? formatSize(props.task.fileSize) : '未知大小'}
                  {' · '}
                  {props.task.url.split(':')[0]?.toUpperCase() ?? ''}
                  {props.task.speed > 0 && ` · ${formatSpeed(props.task.speed)}`}
                </div>
              </Show>
            </div>

            <div
              class="flex-shrink-0"
              style={{
                'min-width': '60px',
                width: '120px',
                'text-align': 'right',
                'font-size': '14px',
                color: '#A0A0B0',
                'font-family': "'Geist Mono', monospace",
              }}
            >
              {(props.task.progress * 100).toFixed(1)}%
            </div>

            <div
              class="flex-shrink-0"
              style={{
                'min-width': '60px',
                width: '100px',
                'text-align': 'right',
                'font-size': '13px',
                color: props.task.status === 'downloading' ? '#00D4AA' : '#A0A0B0',
                'font-family': "'Geist Mono', monospace",
                'overflow': 'hidden',
                'text-overflow': 'ellipsis',
                'white-space': 'nowrap',
              }}
            >
              {formatSpeed(props.task.speed)}
            </div>

            <div
              class="flex-shrink-0"
              style={{
                'min-width': '40px',
                width: '80px',
                'text-align': 'right',
                'font-size': '12px',
                color: getStatusColor(props.task.status),
                'overflow': 'hidden',
                'text-overflow': 'ellipsis',
                'white-space': 'nowrap',
              }}
            >
              {getStatusLabel(props.task.status)}
            </div>
          </div>

          <div
            class="relative overflow-hidden"
            style={{
              height: isCompact() ? '2px' : '3px',
              'margin-top': isCompact() ? '6px' : '8px',
              'border-radius': '9999px',
              background: '#1A1A25',
            }}
          >
            <div
              class={`absolute left-0 top-0 bottom-0${props.task.status === 'downloading' ? ' progress-bar-active' : ''}`}
              style={{
                width: `${props.task.progress * 100}%`,
                'border-radius': '9999px',
                background: props.task.status === 'failed'
                  ? '#EF4444'
                  : props.task.status === 'downloading'
                    ? undefined
                    : 'linear-gradient(90deg, #00D4AA 0%, #00B4D8 100%)',
                transition: 'width 300ms ease-out',
              }}
            />
          </div>
        </div>
      </div>
    </div>
  )
}
