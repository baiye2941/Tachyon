import { For, Show } from 'solid-js'
import {
  PlusIcon, SearchIcon, PauseIcon, PlayIcon, SettingsIcon,
  SelectIcon, CheckboxIcon, XIcon, TrashIcon,
} from './icons'

function getFilterColor(type: string): string {
  switch (type) {
    case 'status': return '#00D4AA'
    case 'type': return '#00B4D8'
    case 'size': return '#F59E0B'
    case 'speed': return '#8B5CF6'
    case 'name': return '#A0A0B0'
    default: return '#A0A0B0'
  }
}

function getFilterBorderColor(type: string): string {
  switch (type) {
    case 'status': return 'rgba(0, 212, 170, 0.3)'
    case 'type': return 'rgba(0, 180, 216, 0.3)'
    case 'size': return 'rgba(245, 158, 11, 0.3)'
    case 'speed': return 'rgba(139, 92, 246, 0.3)'
    case 'name': return 'rgba(255, 255, 255, 0.1)'
    default: return 'rgba(255, 255, 255, 0.1)'
  }
}

interface FilterTag {
  type: string
  value: string
  raw: string
}

interface ToolbarProps {
  searchQuery: string
  onSearchChange: (q: string) => void
  filters: FilterTag[]
  onRemoveFilter: (raw: string) => void
  isMultiSelectMode: boolean
  onToggleMultiSelect: () => void
  selectedCount: number
  onSelectAll: () => void
  onPauseSelected: () => void
  onResumeSelected: () => void
  onDeleteSelected: () => void
  onExitMultiSelect: () => void
  listDensity: 'comfortable' | 'compact'
  onToggleDensity: () => void
  onNewTask: () => void
  onOpenSettings: () => void
}

export default function Toolbar(props: ToolbarProps) {
  return (
    <div
      class="flex items-center justify-between flex-shrink-0"
      style={{
        height: '56px',
        padding: '0 16px',
        'border-bottom': '1px solid rgba(255,255,255,0.05)',
      }}
    >
      {props.isMultiSelectMode ? (
        <div class="flex items-center gap-3 flex-1">
          <button
            class="hover-light flex items-center gap-2"
            style={{
              padding: '6px 12px',
              'border-radius': '6px',
              'font-size': '14px',
              border: 'none',
              cursor: 'pointer',
            }}
            onClick={props.onSelectAll}
          >
            <CheckboxIcon checked={props.selectedCount > 0} />
            <span>全选</span>
          </button>

          <span style={{ 'font-size': '14px', color: '#A0A0B0' }}>
            已选 {props.selectedCount} 项
          </span>

          <div class="flex-1" />

          <button
            class="hover-light flex items-center gap-1"
            style={{
              padding: '6px 12px',
              'border-radius': '6px',
              'font-size': '14px',
              border: 'none',
              cursor: 'pointer',
            }}
            onClick={props.onPauseSelected}
          >
            <PauseIcon />
            <span>暂停</span>
          </button>

          <button
            class="hover-light flex items-center gap-1"
            style={{
              padding: '6px 12px',
              'border-radius': '6px',
              'font-size': '14px',
              border: 'none',
              cursor: 'pointer',
            }}
            onClick={props.onResumeSelected}
          >
            <PlayIcon />
            <span>恢复</span>
          </button>

          <button
            class="hover-danger flex items-center gap-1"
            style={{
              padding: '6px 12px',
              'border-radius': '6px',
              'font-size': '14px',
              border: 'none',
              cursor: 'pointer',
            }}
            onClick={props.onDeleteSelected}
          >
            <TrashIcon />
            <span>删除</span>
          </button>

          <button
            class="hover-light flex items-center gap-1"
            style={{
              padding: '6px 12px',
              'border-radius': '6px',
              'font-size': '14px',
              border: 'none',
              cursor: 'pointer',
            }}
            onClick={props.onExitMultiSelect}
          >
            <XIcon />
            <span>退出</span>
          </button>
        </div>
      ) : (
        <div class="flex items-center gap-3 flex-1">
          <button
            class="btn-primary hover-lift flex items-center gap-2"
            style={{ 'font-size': '14px' }}
            onClick={props.onNewTask}
          >
            <PlusIcon />
            <span>新建下载</span>
          </button>

          <div class="relative flex flex-col gap-2">
            <div class="relative">
              <div
                class="absolute left-3 top-1/2 -translate-y-1/2 pointer-events-none"
                style={{ color: '#6B7280' }}
              >
                <SearchIcon />
              </div>
              <input
                type="text"
                placeholder="搜索任务或设置..."
                value={props.searchQuery}
                onInput={e => props.onSearchChange(e.currentTarget.value)}
                class="input"
                style={{
                  'padding-left': '36px',
                  width: '280px',
                  'font-size': '14px',
                  transition: 'all 200ms ease',
                }}
                onFocus={e => {
                  e.currentTarget.style.width = '320px'
                }}
                onBlur={e => {
                  if (!e.currentTarget.value) {
                    e.currentTarget.style.width = '280px'
                  }
                }}
              />
            </div>

            <Show when={props.filters.length > 0}>
              <div class="flex items-center gap-2 flex-wrap">
                <For each={props.filters}>
                  {(filter) => (
                    <div
                      class="flex items-center gap-1"
                      style={{
                        padding: '2px 8px',
                        'border-radius': '4px',
                        'font-size': '12px',
                        background: 'rgba(255, 255, 255, 0.06)',
                        border: `1px solid ${getFilterBorderColor(filter.type)}`,
                        color: getFilterColor(filter.type),
                      }}
                    >
                      <span>{filter.raw}</span>
                      <button
                        class="flex items-center justify-center"
                        style={{
                          width: '14px',
                          height: '14px',
                          background: 'none',
                          border: 'none',
                          cursor: 'pointer',
                          color: 'inherit',
                          opacity: 0.7,
                        }}
                        onClick={() => props.onRemoveFilter(filter.raw)}
                      >
                        <XIcon />
                      </button>
                    </div>
                  )}
                </For>
              </div>
            </Show>
          </div>

          <div class="flex-1" />

          <button class="icon-btn" title="暂停全部">
            <PauseIcon />
          </button>

          <button class="icon-btn" title="恢复全部">
            <PlayIcon />
          </button>

          <button class="icon-btn" title="设置" onClick={props.onOpenSettings}>
            <SettingsIcon />
          </button>

          <button class="icon-btn" onClick={props.onToggleMultiSelect} title="多选模式">
            <SelectIcon />
          </button>
        </div>
      )}
    </div>
  )
}
