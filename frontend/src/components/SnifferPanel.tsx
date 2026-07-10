import { createSignal, createMemo, For, Show } from 'solid-js'
import type { SnifferResource, CaptureConfig, SnifferResourceType } from '../types'
import {
  CloseIcon, BrowserIcon, VideoIcon, AudioIcon, DocumentIcon,
  ImageIcon, ArchiveIcon, PlusIcon, CheckboxIcon, TrashIcon, LinkIcon,
  ChevronDownIcon,
} from './icons'
import { formatSize } from '../utils/format'
import Button from '../shared/ui/Button'
import { tr } from '../i18n'
import { addToast } from '../stores/toast'
import { requestConfirm } from '../stores/confirm'

const typeColors: Record<string, string> = {
  video: 'var(--color-file-video)',
  audio: 'var(--color-file-audio)',
  document: 'var(--color-file-document)',
  archive: 'var(--color-file-archive)',
  executable: 'var(--color-file-executable)',
  image: 'var(--color-file-image)',
  model: 'var(--color-file-model)',
  other: 'var(--color-file-other)',
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

/** 配置区可切换的资源类型(不含 other,other 无法被白名单过滤) */
const CONFIG_TYPES: SnifferResourceType[] = [
  'video', 'audio', 'document', 'archive', 'executable', 'image', 'model',
]

interface SnifferPanelProps {
  visible: boolean
  resources: SnifferResource[]
  captureConfig: CaptureConfig | null
  onClose: () => void
  onAddDownload: (resource: SnifferResource) => void
  onAddResource: (url: string) => void
  onClearResources: () => void
  onUpdateConfig: (config: CaptureConfig) => void
}

export default function SnifferPanel(props: SnifferPanelProps) {
  const [filterType, setFilterType] = createSignal<string>('all')
  const [selectedIds, setSelectedIds] = createSignal<Set<string>>(new Set())
  const [batchMode, setBatchMode] = createSignal(false)
  const [urlInput, setUrlInput] = createSignal('')
  const [configOpen, setConfigOpen] = createSignal(false)
  const [filterInput, setFilterInput] = createSignal('')

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

  const submitUrl = () => {
    const url = urlInput().trim()
    if (!url) return
    // 简单校验 http(s) 前缀
    if (!/^https?:\/\//i.test(url)) {
      addToast(tr('sniffer.invalidUrl'), 'warning')
      return
    }
    props.onAddResource(url)
    setUrlInput('')
  }

  const handleClear = async () => {
    if (props.resources.length === 0) return
    const result = await requestConfirm({
      title: tr('sniffer.clearConfirmTitle'),
      message: tr('sniffer.clearConfirm'),
      confirmLabel: tr('sniffer.clearConfirmLabel'),
      tone: 'danger',
    })
    if (result.ok) {
      props.onClearResources()
    }
  }

  // —— 捕获配置操作(即时保存) ——
  const toggleType = (type: SnifferResourceType) => {
    const cfg = props.captureConfig
    if (!cfg) return
    const enabled = cfg.enabledTypes.includes(type)
      ? cfg.enabledTypes.filter(t => t !== type)
      : [...cfg.enabledTypes, type]
    props.onUpdateConfig({ ...cfg, enabledTypes: enabled })
  }

  const updateMinSize = (raw: string) => {
    const cfg = props.captureConfig
    if (!cfg) return
    // 输入框单位为 KB,转为字节;空值或非法值回退为 0(不过滤)
    const kb = parseInt(raw, 10)
    const bytes = Number.isNaN(kb) || kb < 0 ? 0 : kb * 1024
    props.onUpdateConfig({ ...cfg, minSize: bytes })
  }

  const addUrlFilter = () => {
    const cfg = props.captureConfig
    if (!cfg) return
    const keyword = filterInput().trim()
    if (!keyword || cfg.urlFilters.includes(keyword)) return
    props.onUpdateConfig({ ...cfg, urlFilters: [...cfg.urlFilters, keyword] })
    setFilterInput('')
  }

  const removeUrlFilter = (keyword: string) => {
    const cfg = props.captureConfig
    if (!cfg) return
    props.onUpdateConfig({
      ...cfg,
      urlFilters: cfg.urlFilters.filter(f => f !== keyword),
    })
  }

  return (
    <div
      class="slide-panel"
      role="dialog"
      aria-modal="true"
      aria-label={tr("sniffer.aria")}
      style={{
        width: 'var(--panel-sniffer-width, 380px)',
        transform: props.visible ? 'translateX(0)' : 'translateX(100%)',
      }}
    >
      {/* Header */}
      <div class="panel-header">
        <div class="panel-title">
          <BrowserIcon />
          <span>{tr("sniffer.title", { count: props.resources.length })}</span>
        </div>
        <div class="flex items-center gap-1">
          <button
            class="icon-btn-sm hover-light"
            title={tr("sniffer.clear")}
            onClick={handleClear}
          >
            <TrashIcon />
          </button>
          <button
            class="icon-btn-sm hover-light"
            onClick={() => props.onClose()}
          >
            <CloseIcon />
          </button>
        </div>
      </div>

      {/* Filter Bar */}
      <div class="flex items-center gap-2 flex-wrap" style={{ padding: '12px 20px', 'border-bottom': '1px solid var(--color-border-subtle)' }}>
        <For each={types()}>
          {(type) => (
            <button
              class={filterType() === type ? 'pill-btn pill-btn-active' : 'pill-btn pill-btn-default'}
              onClick={() => setFilterType(type)}
            >
              {type === 'all' ? tr("common.all") : type}
            </button>
          )}
        </For>
      </div>

      {/* Manual URL input */}
      <div class="flex items-center gap-2" style={{ padding: '10px 20px', 'border-bottom': '1px solid var(--color-border-subtle)' }}>
        <div class="flex-1 flex items-center gap-2" style={{
          padding: '6px 10px',
          'border-radius': '8px',
          'background': 'var(--color-bg-elevated)',
          'border': '1px solid var(--color-border-default)',
        }}>
          <span style={{ color: 'var(--color-text-tertiary)', display: 'flex' }}>
            <LinkIcon />
          </span>
          <input
            type="text"
            value={urlInput()}
            onInput={(e) => setUrlInput(e.currentTarget.value)}
            onKeyDown={(e) => { if (e.key === 'Enter') submitUrl() }}
            placeholder={tr("sniffer.addUrlPlaceholder")}
            style={{
              flex: 1,
              background: 'transparent',
              border: 'none',
              outline: 'none',
              color: 'var(--color-text-title)',
              'font-size': '13px',
            }}
          />
        </div>
        <Button
          variant="ghost"
          size="sm"
          class="flex items-center gap-1"
          style={{ color: 'var(--color-accent-primary)' }}
          onClick={submitUrl}
        >
          <PlusIcon />
          {tr("sniffer.addUrlButton")}
        </Button>
      </div>

      {/* 捕获配置(渐进披露折叠) */}
      <div style={{ 'border-bottom': '1px solid var(--color-border-subtle)' }}>
        <button
          type="button"
          class="detail-disclosure-row"
          aria-expanded={configOpen()}
          aria-controls="sniffer-config"
          onClick={() => setConfigOpen(v => !v)}
          style={{ padding: '8px 20px' }}
        >
          <span class="detail-disclosure-row-label">
            {tr("sniffer.config")}
          </span>
          <ChevronDownIcon
            class={`detail-disclosure-chevron${configOpen() ? " detail-disclosure-chevron--open" : ""}`}
          />
        </button>
        <Show when={configOpen() && props.captureConfig}>
          {(cfg) => (
            <div id="sniffer-config" style={{ padding: '4px 20px 14px' }}>
              {/* 类型白名单 */}
              <div class="section-label" style={{ 'margin-top': '8px' }}>
                {tr("sniffer.config.types")}
              </div>
              <div class="flex items-center gap-2 flex-wrap" style={{ 'margin-top': '6px' }}>
                <For each={CONFIG_TYPES}>
                  {(type) => (
                    <button
                      class={cfg().enabledTypes.includes(type)
                        ? 'pill-btn pill-btn-active'
                        : 'pill-btn pill-btn-default'}
                      onClick={() => toggleType(type)}
                    >
                      {type}
                    </button>
                  )}
                </For>
              </div>

              {/* 最小文件大小 */}
              <div class="section-label" style={{ 'margin-top': '14px' }}>
                {tr("sniffer.config.minSize")}
              </div>
              <div class="flex items-center gap-2" style={{ 'margin-top': '6px' }}>
                <input
                  type="number"
                  class="input"
                  min={0}
                  step={1}
                  value={Math.floor(cfg().minSize / 1024)}
                  onChange={(e) => updateMinSize(e.currentTarget.value)}
                  style={{ width: '120px' }}
                />
                <span class="mono" style={{
                  'font-size': '11px',
                  color: 'var(--color-text-tertiary)',
                  'white-space': 'nowrap',
                }}>
                  KB
                </span>
              </div>

              {/* URL 过滤器 */}
              <div class="section-label" style={{ 'margin-top': '14px' }}>
                {tr("sniffer.config.urlFilters")}
              </div>
              <Show when={cfg().urlFilters.length > 0} fallback={
                <div style={{
                  'font-size': '12px',
                  color: 'var(--color-text-tertiary)',
                  'margin-top': '6px',
                }}>
                  {tr("sniffer.config.urlFilterEmpty")}
                </div>
              }>
                <div class="flex flex-col gap-1" style={{ 'margin-top': '6px' }}>
                  <For each={cfg().urlFilters}>
                    {(keyword) => (
                      <div class="flex items-center justify-between" style={{
                        padding: '4px 10px',
                        'border-radius': '6px',
                        'background': 'var(--color-bg-elevated)',
                      }}>
                        <span class="truncate" style={{
                          'font-size': '12px',
                          color: 'var(--color-text-secondary)',
                        }}>
                          {keyword}
                        </span>
                        <button
                          class="icon-btn-sm hover-light"
                          style={{ width: '24px', height: '24px', 'flex-shrink': 0 }}
                          onClick={() => removeUrlFilter(keyword)}
                        >
                          <CloseIcon />
                        </button>
                      </div>
                    )}
                  </For>
                </div>
              </Show>
              <div class="flex items-center gap-2" style={{ 'margin-top': '8px' }}>
                <input
                  type="text"
                  class="input"
                  value={filterInput()}
                  onInput={(e) => setFilterInput(e.currentTarget.value)}
                  onKeyDown={(e) => { if (e.key === 'Enter') addUrlFilter() }}
                  placeholder={tr("sniffer.config.urlFilterPlaceholder")}
                  style={{ flex: 1, 'font-size': '13px' }}
                />
                <Button
                  variant="ghost"
                  size="sm"
                  class="flex items-center gap-1"
                  style={{ color: 'var(--color-accent-primary)' }}
                  onClick={addUrlFilter}
                >
                  <PlusIcon />
                </Button>
              </div>
            </div>
          )}
        </Show>
      </div>

      {/* Batch toggle */}
      <div class="flex items-center justify-between" style={{ padding: '8px 20px' }}>
        <Button
          variant="ghost"
          size="sm"
          class="flex items-center gap-1"
          style={{ color: batchMode() ? 'var(--color-accent-primary)' : 'var(--color-text-tertiary)' }}
          onClick={() => {
            const wasBatch = batchMode()
            setBatchMode(v => !v)
            if (wasBatch) setSelectedIds(new Set<string>())
          }}
        >
          <CheckboxIcon checked={batchMode()} />
          <span>{tr("sniffer.batchSelect")}</span>
        </Button>
      </div>

      {/* Resource List */}
      <div class="flex-1 scroll-container" style={{ padding: '0 12px 12px' }}>
        <Show when={filteredResources().length > 0} fallback={
          <div class="flex flex-col items-center justify-center" style={{
            padding: '48px 20px',
            color: 'var(--color-text-tertiary)',
            'text-align': 'center',
            'font-size': '13px',
          }}>
            <div style={{ opacity: 0.5, 'margin-bottom': '12px', display: 'flex' }}>
              <BrowserIcon />
            </div>
            {tr("sniffer.empty")}
          </div>
        }>
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
                    background: isSelected() ? 'var(--color-accent-soft)' : 'transparent',
                    'border-left': isSelected() ? '2px solid var(--color-accent-primary)' : '2px solid transparent',
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
                      color: typeColors[resource.type] || 'var(--color-file-other)',
                    }}
                  >
                    <Show when={batchMode()} fallback={<Icon />}>
                      <div style={{ color: isSelected() ? 'var(--color-accent-primary)' : 'var(--color-text-tertiary)' }}>
                        <CheckboxIcon checked={isSelected()} />
                      </div>
                    </Show>
                  </div>

                  {/* Info */}
                  <div class="flex-1 min-w-0">
                    <div class="truncate" style={{ 'font-size': '14px', color: 'var(--color-text-title)', 'font-weight': 500 }}>
                      {resource.name}
                    </div>
                    <div style={{ 'font-size': '12px', color: 'var(--color-text-tertiary)', 'margin-top': '2px' }}>
                      {formatSize(resource.size)} · {resource.contentType || resource.type}
                    </div>
                    <Show when={resource.sourcePage}>
                      <div class="truncate" style={{ 'font-size': '12px', color: 'var(--color-text-tertiary)', 'margin-top': '2px' }}>
                        {resource.sourcePage}
                      </div>
                    </Show>
                  </div>

                  {/* Add button */}
                  <Show when={!batchMode()}>
                    <Button
                      variant="ghost"
                      size="sm"
                      class="hover-accent"
                      style={{ color: 'var(--color-accent-primary)' }}
                      onClick={(e) => {
                        e.stopPropagation()
                        props.onAddDownload(resource)
                      }}
                    >
                      <PlusIcon />
                    </Button>
                  </Show>
                </div>
              )
            }}
          </For>
        </Show>
      </div>

      {/* Batch Actions */}
      <Show when={batchMode() && selectedIds().size > 0}>
        <div
          class="flex items-center justify-between"
          style={{
            padding: '12px 20px',
            'border-top': '1px solid var(--color-border-default)',
            background: 'var(--color-bg-elevated)',
          }}
        >
          <Button
            variant="ghost"
            size="sm"
            class="flex items-center gap-1"
            onClick={selectAll}
          >
            <CheckboxIcon />
            <span>{tr("common.selectAll")}</span>
          </Button>
          <span style={{ 'font-size': '12px', color: 'var(--color-text-tertiary)' }}>
            {tr("batch.selectedCount", { count: selectedIds().size })}
          </span>
          <Button
            variant="primary"
            size="sm"
            class="flex items-center gap-1"
            onClick={() => {
              const selected = props.resources.filter(r => selectedIds().has(r.id))
              selected.forEach(r => props.onAddDownload(r))
              setSelectedIds(new Set<string>())
            }}
          >
            <PlusIcon />
            {tr("sniffer.batchAdd", { count: selectedIds().size })}
          </Button>
        </div>
      </Show>
    </div>
  )
}
