import { createMemo, For, Show } from 'solid-js'
import type { TaskInfo, ListDensity } from '../types'
import { CheckboxIcon } from './icons'
import { COLUMN_WIDTH } from './taskColumns'
import { formatSize, formatSpeed, getFileType, getStatusLabel } from '../utils/format'
import { tr } from '../i18n'

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

/**
 * 搜索高亮文本组件。
 *
 * 用 String.split(regex) 单次分割(O(n))替代原先的 indexOf 循环(O(n×m)),
 * 大小写不敏感由正则 i 标志处理,无需预先 toLowerCase 整串。
 * 无 query 时返回 null,fallback 直接渲染原文,避免无谓的数组创建。
 *
 * 高亮用 <mark class="search-highlight"> 语义化标签,样式走 CSS token。
 */
function HighlightedText(props: { text: string; query: string }) {
  const segments = createMemo(() => {
    const query = props.query.trim()
    if (!query) return null // null = 无高亮,直接渲染原文

    try {
      // 转义正则特殊字符,避免恶意输入触发 ReDoS
      const escaped = query.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
      const regex = new RegExp(`(${escaped})`, 'gi')
      const result = props.text.split(regex)
      // split 带捕获组会保留分隔符:奇数下标 = 匹配项
      return result.length > 1 ? result : null
    } catch {
      return null // 非法正则回退
    }
  })

  return (
    <Show when={segments()} fallback={props.text}>
      {(segs) => (
        <For each={segs()}>
          {(seg, i) => {
            // eslint-disable-next-line solid/reactivity -- <For> 回调是 tracked scope,i() 安全
            const isMatch = i() % 2 === 1
            return isMatch ? (
              <mark class="search-highlight">{seg}</mark>
            ) : (
              seg
            )
          }}
        </For>
      )}
    </Show>
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
    return tr('taskList.aria.taskItem', { name: props.task.fileName, progress, status })
  }

  return (
    <div
      role="button"
      tabindex="0"
      aria-label={ariaLabel()}
      class="cursor-pointer transition-all duration-150 hover-lift-sm task-item-enter focus:outline-none focus-visible:focus-ring"
      style={{
        padding: isCompact() ? '6px 16px' : '12px 16px',
        background: props.isMultiSelected
          ? 'var(--color-accent-soft)'
          : props.isSelected
            ? 'var(--color-accent-faint)'
            : 'transparent',
        'border-left': props.isMultiSelected
          ? '2px solid var(--color-accent-primary)'
          : '2px solid transparent',
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
            aria-label={tr('taskList.aria.selectTask', { name: props.task.fileName })}
            style={{
              width: '20px',
              height: '20px',
              color: props.isMultiSelected
                ? 'var(--color-accent-primary)'
                : 'var(--color-text-tertiary)',
            }}
          >
            <CheckboxIcon checked={props.isMultiSelected} />
          </div>
        </Show>

        <div
          class="flex items-center justify-center flex-shrink-0"
          style={{
            width: isCompact() ? '28px' : '40px',
            height: isCompact() ? '28px' : '40px',
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
                  'font-size': isCompact() ? '13px' : '14px',
                  'font-weight': 500,
                  color: 'var(--color-text-title)',
                }}
              >
                <HighlightedText text={props.task.fileName} query={props.searchQuery || ''} />
              </div>
              {/* compact 模式隐藏元信息行,换取信息密度 */}
              <Show when={!isCompact()}>
                <div
                  class="truncate"
                  style={{
                    'font-size': '12px',
                    color: 'var(--color-text-secondary)',
                    'margin-top': '2px',
                  }}
                >
                  {props.task.fileSize ? formatSize(props.task.fileSize) : tr('taskList.unknownSize')}
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
                width: COLUMN_WIDTH.progress,
                'text-align': 'right',
                'font-size': isCompact() ? '12px' : '14px',
                color: 'var(--color-text-secondary)',
                'font-family': "'Geist Mono', monospace",
              }}
            >
              {(props.task.progress * 100).toFixed(1)}%
            </div>

            <div
              class="flex-shrink-0"
              style={{
                'min-width': '60px',
                width: COLUMN_WIDTH.speed,
                'text-align': 'right',
                'font-size': isCompact() ? '12px' : '13px',
                // 下载中速度用 Neon Cyan(能量隐喻),其余中性灰
                color:
                  props.task.status === 'downloading'
                    ? 'var(--color-speed-active)'
                    : 'var(--color-text-secondary)',
                'font-family': "'Geist Mono', monospace",
                'overflow': 'hidden',
                'text-overflow': 'ellipsis',
                'white-space': 'nowrap',
              }}
            >
              {formatSpeed(props.task.speed)}
            </div>

            <div
              class="flex-shrink-0 flex justify-end"
              style={{
                'min-width': '48px',
                width: COLUMN_WIDTH.status,
              }}
            >
              <span
                class={`status-badge status-badge--${props.task.status}`}
                title={getStatusLabel(props.task.status)}
              >
                {getStatusLabel(props.task.status)}
              </span>
            </div>
          </div>

          <div
            class="relative overflow-hidden"
            style={{
              height: '4px',
              'margin-top': isCompact() ? '4px' : '8px',
              'border-radius': '9999px',
              background: 'var(--color-bg-tertiary)',
            }}
          >
            <div
              class={`absolute left-0 top-0 bottom-0${props.task.status === 'downloading' ? ' progress-bar-active' : ''}`}
              style={{
                width: `${props.task.progress * 100}%`,
                'border-radius': '9999px',
                // spec 8.1:失败=红,完成=翠绿,下载中=accent shimmer,暂停=灰,其他=accent 渐变
                background:
                  props.task.status === 'failed'
                    ? 'var(--color-status-error)'
                    : props.task.status === 'completed'
                      ? 'var(--color-status-completed)'
                      : props.task.status === 'downloading'
                        ? undefined
                        : props.task.status === 'paused'
                          ? 'var(--color-status-paused)'
                          : 'linear-gradient(90deg, var(--color-accent-primary) 0%, var(--color-accent-tertiary) 100%)',
                transition: 'width 300ms ease-out',
              }}
            />
          </div>
        </div>
      </div>
    </div>
  )
}
