import { For, Show } from 'solid-js'
import type { TaskInfo, ListDensity } from '../types'
import TaskItem from './TaskItem'

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

      {/* Task Items */}
      <div class="flex-1 overflow-y-auto">
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
          <For each={props.tasks}>
            {(task, index) => (
              <TaskItem
                task={task}
                isSelected={props.selectedTaskId === task.id}
                isMultiSelected={props.selectedTaskIds.has(task.id)}
                isMultiSelectMode={props.isMultiSelectMode}
                onClick={() => props.onTaskClick(task.id)}
                onContextMenu={e => props.onTaskContextMenu?.(e, task.id)}
                density={props.density}
                searchQuery={props.searchQuery}
                staggerIndex={index()}
              />
            )}
          </For>
        </Show>
      </div>
    </div>
  )
}
