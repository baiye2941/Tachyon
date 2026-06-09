import { For, Show, createSignal, createMemo, createEffect, onMount, onCleanup } from 'solid-js'
import type { TaskInfo, ListDensity } from '../types'
import TaskItem from './TaskItem'

/** Fixed row heights per density mode (px) */
const ITEM_HEIGHTS: Record<ListDensity, number> = {
  comfortable: 80,
  compact: 52,
}

/** Number of off-screen buffer items rendered above/below the viewport */
const BUFFER_COUNT = 3

interface TaskListProps {
  tasks: TaskInfo[]
  selectedTaskId: string | null
  onTaskClick: (taskId: string) => void
  onTaskContextMenu?: (e: MouseEvent, taskId: string) => void
  isMultiSelectMode: boolean
  selectedTaskIds: Set<string>
  density: ListDensity
  searchQuery?: string
}

export default function TaskList(props: TaskListProps) {
  let scrollContainerRef: HTMLDivElement | undefined
  let rafId: number | null = null

  // ── Virtual-scroll reactive state ──────────────────────────────
  const [scrollTop, setScrollTop] = createSignal(0)
  const [containerHeight, setContainerHeight] = createSignal(500)

  const itemHeight = createMemo(() => ITEM_HEIGHTS[props.density])

  const totalHeight = createMemo(() => props.tasks.length * itemHeight())

  /** How many items fit in the visible viewport */
  const visibleCount = createMemo(
    () => Math.ceil(containerHeight() / itemHeight()) + 1,
  )

  /** First index in the render window (including buffer) */
  const startIndex = createMemo(() => {
    const raw = Math.floor(scrollTop() / itemHeight()) - BUFFER_COUNT
    return Math.max(0, raw)
  })

  /** Last index (exclusive) in the render window (including buffer) */
  const endIndex = createMemo(() => {
    const raw =
      Math.floor(scrollTop() / itemHeight()) + visibleCount() + BUFFER_COUNT
    return Math.min(props.tasks.length, raw)
  })

  /** Y-offset for the inner positioning container */
  const offsetY = createMemo(() => startIndex() * itemHeight())

  /** The subset of tasks currently rendered (<For> reconciles by identity) */
  const visibleTasks = createMemo(() => props.tasks.slice(startIndex(), endIndex()))

  // ── Scroll handler (RAF-throttled) ─────────────────────────────
  const handleScroll = () => {
    if (rafId !== null) return
    rafId = requestAnimationFrame(() => {
      rafId = null
      if (scrollContainerRef) {
        setScrollTop(scrollContainerRef.scrollTop)
      }
    })
  }

  // ── Measure viewport height ────────────────────────────────────
  const measureHeight = () => {
    if (scrollContainerRef) {
      setContainerHeight(scrollContainerRef.clientHeight)
    }
  }

  let resizeObserver: ResizeObserver | undefined

  onMount(() => {
    measureHeight()
    if (scrollContainerRef) {
      resizeObserver = new ResizeObserver(measureHeight)
      resizeObserver.observe(scrollContainerRef)
    }
  })

  onCleanup(() => {
    if (rafId !== null) cancelAnimationFrame(rafId)
    resizeObserver?.disconnect()
  })

  // ── Scroll selected task into view ─────────────────────────────
  const scrollToTask = (taskId: string) => {
    const idx = props.tasks.findIndex(t => t.id === taskId)
    if (idx < 0 || !scrollContainerRef) return
    const top = idx * itemHeight()
    const bottom = top + itemHeight()
    const viewTop = scrollContainerRef.scrollTop
    const viewBottom = viewTop + scrollContainerRef.clientHeight
    if (top < viewTop) {
      scrollContainerRef.scrollTop = top
    } else if (bottom > viewBottom) {
      scrollContainerRef.scrollTop = bottom - scrollContainerRef.clientHeight
    }
  }

  // Auto-scroll when the externally-selected task changes
  createEffect(() => {
    const id = props.selectedTaskId
    if (id) scrollToTask(id)
  })

  return (
    <div class="flex-1 flex flex-col min-w-0 overflow-hidden">
      {/* List Header */}
      <div
        class="flex items-center flex-shrink-0"
        style={{
          height: '36px',
          padding: '0 16px',
          background: 'rgba(10,10,15,0.8)',
          'backdrop-filter': 'blur(8px)',
          'border-bottom': '1px solid rgba(255,255,255,0.05)',
          'font-size': '12px',
          color: '#6B7280',
          'font-weight': 600,
          'text-transform': 'uppercase',
          'letter-spacing': '0.5px',
        }}
      >
        <div class="flex-1">文件名</div>
        <div style={{ width: '120px', 'text-align': 'right' }}>进度</div>
        <div style={{ width: '100px', 'text-align': 'right' }}>速度</div>
        <div style={{ width: '80px', 'text-align': 'right' }}>状态</div>
      </div>

      {/* Virtual-scroll viewport */}
      <div
        ref={scrollContainerRef}
        class="flex-1 overflow-y-auto"
        onScroll={handleScroll}
      >
        <Show
          when={props.tasks.length > 0}
          fallback={
            <div class="flex flex-col items-center justify-center h-full gap-4">
              <div
                style={{
                  width: '120px',
                  height: '120px',
                  color: '#6B7280',
                  opacity: 0.3,
                }}
              >
                <svg width="120" height="120" viewBox="0 0 120 120" fill="none">
                  <path
                    d="M60 10 L110 60 L60 110 L10 60 Z"
                    stroke="url(#grad)"
                    stroke-width="2"
                    opacity="0.5"
                  />
                  <circle cx="60" cy="60" r="15" fill="url(#grad)" opacity="0.3" />
                  <defs>
                    <linearGradient id="grad" x1="0%" y1="0%" x2="100%" y2="100%">
                      <stop offset="0%" stop-color="#00D4AA" />
                      <stop offset="100%" stop-color="#00B4D8" />
                    </linearGradient>
                  </defs>
                </svg>
              </div>
              <div class="text-center">
                <p style={{ 'font-size': '16px', color: '#A0A0B0', 'margin-bottom': '8px' }}>
                  暂无下载任务
                </p>
                <p style={{ 'font-size': '14px', color: '#6B7280' }}>
                  点击「新建下载」或拖拽链接到此处
                </p>
              </div>
            </div>
          }
        >
          {/* Outer wrapper: sets total scrollable height via spacer */}
          <div style={{ position: 'relative', height: `${totalHeight()}px` }}>
            {/* Inner wrapper: offset to the visible window */}
            <div
              style={{
                position: 'absolute',
                top: 0,
                left: 0,
                right: 0,
                transform: `translateY(${offsetY()}px)`,
              }}
            >
              <For each={visibleTasks()}>
                {(task, visibleIndex) => (
                  <TaskItem
                    task={task}
                    isSelected={props.selectedTaskId === task.id}
                    isMultiSelected={props.selectedTaskIds.has(task.id)}
                    isMultiSelectMode={props.isMultiSelectMode}
                    onClick={() => props.onTaskClick(task.id)}
                    onContextMenu={e => props.onTaskContextMenu?.(e, task.id)}
                    density={props.density}
                    searchQuery={props.searchQuery}
                    staggerIndex={visibleIndex()}
                  />
                )}
              </For>
            </div>
          </div>
        </Show>
      </div>
    </div>
  )
}
