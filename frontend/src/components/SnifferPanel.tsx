import { createSignal, createMemo, For, Show } from 'solid-js'
import type { SnifferResource } from '../types'
import {
  CloseIcon, BrowserIcon, VideoIcon, AudioIcon, DocumentIcon,
  ImageIcon, ArchiveIcon, PlusIcon, CheckboxIcon,
} from './icons'
import { formatSize } from '../utils/format'

const typeColors: Record<string, string> = {
  video: '#F59E0B',
  audio: '#8B5CF6',
  document: '#3B82F6',
  archive: '#F97316',
  executable: '#6B7280',
  image: '#10B981',
  other: '#6B7280',
}

const typeIcons: Record<string, () => ReturnType<typeof VideoIcon>> = {
  video: () => <VideoIcon />,
  audio: () => <AudioIcon />,
  document: () => <DocumentIcon />,
  archive: () => <ArchiveIcon />,
  executable: () => <DocumentIcon />,
  image: () => <ImageIcon />,
  other: () => <DocumentIcon />,
}

interface SnifferPanelProps {
  visible: boolean
  resources: SnifferResource[]
  onClose: () => void
  onAddDownload: (resource: SnifferResource) => void
}

export default function SnifferPanel(props: SnifferPanelProps) {
  const [filterType, setFilterType] = createSignal<string>('all')
  const [selectedIds, setSelectedIds] = createSignal<Set<string>>(new Set())
  const [batchMode, setBatchMode] = createSignal(false)

  const types = () => ['all', 'video', 'audio', 'document', 'archive', 'executable', 'image', 'other'] as const

  const filteredResources = createMemo(() => {
    const ft = filterType()
    if (ft === 'all') return props.resources
    return props.resources.filter(r => r.type === ft)
  })

  const toggleSelect = (id: string) => {
    setSelectedIds(prev => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  const selectAll = () => {
    const ids = filteredResources().map(r => r.id)
    const allSelected = ids.every(id => selectedIds().has(id))
    if (allSelected) {
      setSelectedIds(prev => {
        const next = new Set(prev)
        ids.forEach(id => next.delete(id))
        return next
      })
    } else {
      setSelectedIds(prev => {
        const next = new Set(prev)
        ids.forEach(id => next.add(id))
        return next
      })
    }
  }

  return (
    <div
      class="slide-panel"
      style={{
        width: '380px',
        transform: props.visible ? 'translateX(0)' : 'translateX(100%)',
      }}
    >
      {/* Header */}
      <div class="panel-header">
        <div class="panel-title">
          <BrowserIcon />
          <span>嗅探到 {props.resources.length} 个资源</span>
        </div>
        <button
          class="icon-btn-sm hover-light"
          onClick={() => props.onClose()}
        >
          <CloseIcon />
        </button>
      </div>

      {/* Filter Bar */}
      <div class="flex items-center gap-2 flex-wrap" style={{ padding: '12px 20px', 'border-bottom': '1px solid rgba(255,255,255,0.05)' }}>
        <For each={types()}>
          {(type) => (
            <button
              class={filterType() === type ? 'pill-btn pill-btn-active' : 'pill-btn pill-btn-default'}
              onClick={() => setFilterType(type)}
            >
              {type === 'all' ? '全部' : type}
            </button>
          )}
        </For>
      </div>

      {/* Batch toggle */}
      <div class="flex items-center justify-between" style={{ padding: '8px 20px' }}>
        <button
          class="flex items-center gap-1"
          style={{
            'font-size': '12px',
            color: batchMode() ? '#00D4AA' : '#6B7280',
            background: 'none',
            border: 'none',
            cursor: 'pointer',
          }}
          onClick={() => {
            setBatchMode(v => !v)
            if (batchMode()) setSelectedIds(new Set<string>())
          }}
        >
          <CheckboxIcon checked={batchMode()} />
          <span>批量选择</span>
        </button>
      </div>

      {/* Resource List */}
      <div class="flex-1 overflow-y-auto" style={{ padding: '0 12px 12px' }}>
        <For each={filteredResources()}>
          {(resource, index) => {
            const isSelected = () => selectedIds().has(resource.id)
            const Icon = (typeIcons[resource.type] ?? typeIcons['other'])!
            return (
              <div
                class="flex items-start gap-3 cursor-pointer hover-row"
                style={{
                  padding: '10px 12px',
                  'border-radius': '8px',
                  background: isSelected() ? 'rgba(0, 212, 170, 0.06)' : 'transparent',
                  'border-left': isSelected() ? '2px solid #00D4AA' : '2px solid transparent',
                  transition: 'all 150ms ease',
                  animation: `card-fade-in 200ms ease forwards`,
                  'animation-delay': `${index() * 40}ms`,
                  opacity: 0,
                }}
                onClick={() => {
                  if (batchMode()) {
                    toggleSelect(resource.id)
                  }
                }}
              >
                {/* Icon */}
                <div
                  class="flex-shrink-0 flex items-center justify-center"
                  style={{
                    width: '32px',
                    height: '32px',
                    color: typeColors[resource.type] || '#6B7280',
                  }}
                >
                  <Show when={batchMode()} fallback={<Icon />}>
                    <div style={{ color: isSelected() ? '#00D4AA' : '#6B7280' }}>
                      <CheckboxIcon checked={isSelected()} />
                    </div>
                  </Show>
                </div>

                {/* Info */}
                <div class="flex-1 min-w-0">
                  <div class="truncate" style={{ 'font-size': '14px', color: '#F0F0F5', 'font-weight': 500 }}>
                    {resource.name}
                  </div>
                  <div style={{ 'font-size': '12px', color: '#6B7280', 'margin-top': '2px' }}>
                    {formatSize(resource.size)} · {resource.contentType || resource.type}
                  </div>
                  <Show when={resource.sourcePage}>
                    <div class="truncate" style={{ 'font-size': '12px', color: '#4A4A5A', 'margin-top': '2px' }}>
                      {resource.sourcePage}
                    </div>
                  </Show>
                </div>

                {/* Add button */}
                <Show when={!batchMode()}>
                  <button
                    class="hover-accent"
                    style={{
                      padding: '4px 10px',
                      'border-radius': '6px',
                      'font-size': '12px',
                      background: 'rgba(0, 212, 170, 0.08)',
                      color: '#00D4AA',
                      border: 'none',
                      cursor: 'pointer',
                      transition: 'all 150ms ease',
                    }}
                    onClick={(e) => {
                      e.stopPropagation()
                      props.onAddDownload(resource)
                    }}
                  >
                    <PlusIcon />
                  </button>
                </Show>
              </div>
            )
          }}
        </For>
      </div>

      {/* Batch Actions */}
      <Show when={batchMode() && selectedIds().size > 0}>
        <div
          class="flex items-center justify-between"
          style={{
            padding: '12px 20px',
            'border-top': '1px solid rgba(255,255,255,0.08)',
            background: 'rgba(18, 18, 26, 0.95)',
          }}
        >
          <button
            class="flex items-center gap-1"
            style={{
              'font-size': '13px',
              color: '#A0A0B0',
              background: 'none',
              border: 'none',
              cursor: 'pointer',
            }}
            onClick={selectAll}
          >
            <CheckboxIcon />
            <span>全选</span>
          </button>
          <span style={{ 'font-size': '12px', color: '#6B7280' }}>
            已选 {selectedIds().size} 项
          </span>
          <button
            class="flex items-center gap-1"
            style={{
              padding: '6px 14px',
              'border-radius': '8px',
              'font-size': '13px',
              background: '#00D4AA',
              color: '#0A0A0F',
              border: 'none',
              cursor: 'pointer',
              'font-weight': 600,
            }}
            onClick={() => {
              const selected = props.resources.filter(r => selectedIds().has(r.id))
              selected.forEach(r => props.onAddDownload(r))
              setSelectedIds(new Set<string>())
            }}
          >
            <PlusIcon />
            批量添加 ({selectedIds().size})
          </button>
        </div>
      </Show>
    </div>
  )
}
