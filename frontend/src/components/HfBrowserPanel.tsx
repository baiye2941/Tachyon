import { errorMessage } from "../utils/appError";
import { createSignal, Show, For, createMemo, untrack } from 'solid-js'
import { api } from '../api/invoke'
import { $hub, listRepoFiles, clearRepoFiles } from '../stores/hub'
import { addToast } from '../stores/toast'
import { refreshTaskList } from '../stores/downloads'
import type { HubFileInfo } from '../types'
import { CloseIcon, SearchIcon, CheckboxIcon, ArrowDownIcon, ChevronDownIcon, FileIcon, HubIcon } from './icons'
import { detectFormat, detectQuant, isModelWeight, isLargeFile, type QuantLevel } from '../utils/modelMeta'
import { buildTree, countByType, type TreeNode } from '../utils/hfTree'
import { buildHfMirrorUrl } from '../utils/hfMirror'
import Button from '../shared/ui/Button'
import EmptyState from '../shared/ui/EmptyState'
import ErrorState from '../shared/ui/ErrorState'
import { tr } from '../i18n'
import { useFocusTrap } from '../hooks/useFocusTrap'

interface HfBrowserPanelProps {
  visible: boolean
  onClose: () => void
}

function formatSize(bytes: number | null): string {
  if (bytes === null || bytes === 0) return '--'
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`
}

/**
 * 量化标签按 tier 差异化着色(Iteration 06 VL-2)。
 * tiny(低质量小体积)中性灰;small/medium(推荐平衡点)accent-primary;
 * large(高精度大体积)accent-secondary。用户扫一眼即知推荐档。
 */
function quantTierClass(quant: QuantLevel): string {
  switch (quant.tier) {
    case 'tiny':
      return 'quant-tag-tiny'
    case 'large':
      return 'quant-tag-large'
    default:
      return 'quant-tag-balanced'
  }
}

/** 筛选档(Iteration 06 DI-5) */
type FilterKey = 'all' | 'gguf' | 'safetensors' | 'large'

/** 文件是否符合当前筛选 + 搜索 */
function matchesFilter(f: HubFileInfo, filter: FilterKey, query: string): boolean {
  if (f.type === 'directory') return false
  if (filter === 'gguf' && !f.path.toLowerCase().endsWith('.gguf')) return false
  if (filter === 'safetensors' && !f.path.toLowerCase().endsWith('.safetensors')) return false
  if (filter === 'large' && !isLargeFile(f.size)) return false
  if (query && !f.path.toLowerCase().includes(query)) return false
  return true
}

/** 递归树节点组件(支持 ARIA tree + 键盘导航,Iteration 06 AA-1) */
function TreeNodeItem(props: {
  node: TreeNode
  repoId: string
  revision: string
  onDownload: (path: string) => void
  depth: number
  isSelected: (path: string) => boolean
  onToggleSelect: (path: string) => void
  /** 当前节点 path 是否匹配筛选(非匹配降透明度) */
  isMatched: () => boolean
}) {
  // 初始展开:根层目录(depth<1)默认展开。untrack 明确表示仅作初始值读取一次,
  // 非响应式追踪(节点 depth/isDirectory 在生命周期内不变)。
  const [expanded, setExpanded] = createSignal(
    untrack(() => props.node.isDirectory && props.depth < 1),
  )

  const handleDownload = async () => {
    try {
      const url = await api.getHfDownloadUrl(props.repoId, props.node.path, props.revision || undefined)
      if (url) {
        await api.createTask(url)
        refreshTaskList()
        addToast(tr('toast.hubAddedDownload', { name: props.node.name }), 'success')
      }
    } catch (e) {
      addToast(tr('toast.hubDownloadFailed', { error: errorMessage(e) }), 'error')
    }
    props.onDownload(props.node.path)
  }

  /** 键盘导航(Iteration 06 II-3):←→ 展开/折叠、Space 勾选、Enter 下载。
   *  焦点移动用浏览器原生 Tab 流(每个 treeitem tabindex=0),完整 roving
   *  tabindex(↑↓ 在节点间移动)留作后续增强。 */
  const handleKeyDown = (e: KeyboardEvent) => {
    if (props.node.isDirectory) {
      if (e.key === 'ArrowRight' && !expanded()) { e.preventDefault(); setExpanded(true); return }
      if (e.key === 'ArrowLeft' && expanded()) { e.preventDefault(); setExpanded(false); return }
    }
    if (e.key === ' ' || e.key === 'Spacebar') {
      if (!props.node.isDirectory) {
        e.preventDefault()
        props.onToggleSelect(props.node.path)
      }
    }
    if (e.key === 'Enter') {
      if (!props.node.isDirectory) { e.preventDefault(); void handleDownload() }
    }
  }

  return (
    <div>
      <div
        class="flex items-center gap-2 cursor-pointer select-none hf-tree-row"
        classList={{ 'hf-tree-row-dimmed': !props.isMatched() }}
        role="treeitem"
        aria-level={props.depth + 1}
        aria-expanded={props.node.isDirectory ? expanded() : undefined}
        aria-selected={props.isSelected(props.node.path)}
        tabindex={props.node.isDirectory ? 0 : 0}
        style={{
          padding: '4px 8px',
          'border-radius': '4px',
          'padding-left': `${props.depth * 16 + 8}px`,
          'font-size': '13px',
          transition: 'background 100ms ease',
          outline: 'none',
        }}
        onClick={() => props.node.isDirectory && setExpanded((v) => !v)}
        onKeyDown={handleKeyDown}
      >
        {/* 多选勾选框(仅文件) */}
        <Show when={!props.node.isDirectory}>
          <div
            class="flex-shrink-0"
            style={{
              width: '16px',
              height: '16px',
              color: props.isSelected(props.node.path) ? 'var(--color-accent-primary)' : 'var(--color-text-tertiary)',
            }}
            onClick={(e) => {
              e.stopPropagation()
              props.onToggleSelect(props.node.path)
            }}
          >
            <CheckboxIcon checked={props.isSelected(props.node.path)} />
          </div>
        </Show>
        {/* 展开/折叠图标 */}
        <Show when={props.node.isDirectory}>
          <ChevronDownIcon class={`flex-shrink-0 hf-chevron ${expanded() ? 'hf-chevron-expanded' : ''}`} />
        </Show>
        <Show when={!props.node.isDirectory}>
          <FileIcon class="flex-shrink-0 hf-file-icon" />
        </Show>

        {/* 名称 */}
        <span
          class="truncate flex-1 min-w-0"
          style={{
            color: props.node.isDirectory ? 'var(--color-text-title)' : 'var(--color-text-primary)',
            'font-weight': props.node.isDirectory ? 500 : 400,
          }}
        >
          {props.node.name}
        </span>

        {/* GGUF 量化标签(tier 差异化) */}
        <Show when={!props.node.isDirectory && isModelWeight(props.node.name)}>
          {(() => {
            const quant = detectQuant(props.node.name)
            return (
              <Show when={quant}>
                <span class={`quant-tag ${quantTierClass(quant!)}`} style={{ 'flex-shrink': 0 }}>
                  {quant!.label}
                </span>
              </Show>
            )
          })()}
        </Show>

        {/* 大文件标记 */}
        <Show when={!props.node.isDirectory && props.node.size && isLargeFile(props.node.size)}>
          <span style={{ 'font-size': '10px', color: 'var(--color-warning)', 'flex-shrink': 0 }}>
            {tr("hub.largeTag")}
          </span>
        </Show>

        {/* LFS 标签 */}
        <Show when={props.node.lfs}>
          <span class="lfs-tag" style={{ 'flex-shrink': 0 }}>
            LFS
          </span>
        </Show>

        {/* 大小 */}
        <Show when={!props.node.isDirectory}>
          <span
            class="mono"
            style={{ 'font-size': '11px', color: 'var(--color-text-tertiary)', 'flex-shrink': 0 }}
          >
            {formatSize(props.node.size ?? props.node.lfs?.size ?? null)}
          </span>

          {/* 下载按钮 */}
          <Button
            variant="ghost"
            size="sm"
            aria-label={tr("hub.aria.downloadFile", { name: props.node.name })}
            onClick={handleDownload}
          >
            <ArrowDownIcon />
          </Button>
        </Show>
      </div>

      {/* 子节点 */}
      <Show when={props.node.isDirectory && expanded()}>
        <For each={props.node.children}>
          {(child) => (
            <TreeNodeItem
              node={child}
              repoId={props.repoId}
              revision={props.revision}
              onDownload={props.onDownload}
              depth={props.depth + 1}
              isSelected={props.isSelected}
              onToggleSelect={props.onToggleSelect}
              isMatched={props.isMatched}
            />
          )}
        </For>
      </Show>
    </div>
  )
}

export default function HfBrowserPanel(props: HfBrowserPanelProps) {
  const [repoId, setRepoId] = createSignal('')
  const [revision, setRevision] = createSignal('main')
  const [browsed, setBrowsed] = createSignal(false)
  const [selectedPaths, setSelectedPaths] = createSignal<Set<string>>(new Set())
  const [batchDownloading, setBatchDownloading] = createSignal(false)
  const [filter, setFilter] = createSignal<FilterKey>('all')
  const [searchInput, setSearchInput] = createSignal('')
  const repoFiles = () => $hub.repoFiles() ?? []
  const loading = () => $hub.loading()
  const error = () => $hub.error()
  let inputRef: HTMLInputElement | undefined

  // 焦点陷阱:与 ConfirmDialog/NewTaskModal 一致,捕获 Tab/Esc,防止焦点逃逸到背景
  let panelRef: HTMLDivElement | undefined
  useFocusTrap({
    active: () => props.visible,
    container: () => panelRef,
    onEscape: () => props.onClose(),
  })

  const handleBrowse = async () => {
    const id = repoId().trim()
    if (!id) {
      addToast(tr('toast.hubEnterRepoId'), 'error')
      return
    }
    if (!id.includes('/') || id.split('/').length !== 2) {
      addToast(tr('toast.hubInvalidId'), 'error')
      return
    }
    await listRepoFiles(id, revision() || undefined)
    setBrowsed(true)
  }

  const handleRetry = () => {
    clearRepoFiles()
    void handleBrowse()
  }

  // ── 多选操作 ──────────────────────────────────────────────
  const toggleSelect = (path: string) => {
    setSelectedPaths((prev) => {
      const next = new Set(prev)
      if (next.has(path)) next.delete(path)
      else next.add(path)
      return next
    })
  }

  /** 智能选择:GGUF 优先 Q4_K_M,safetensors 全选 */
  const smartSelect = () => {
    const modelFiles = (repoFiles() ?? []).filter(
      (f: HubFileInfo) => f.type !== 'directory' && isModelWeight(f.path),
    )
    if (modelFiles.length === 0) {
      addToast(tr('toast.hubNoWeights'), 'info')
      return
    }
    const ggufFiles = modelFiles.filter((f: HubFileInfo) => f.path.endsWith('.gguf'))
    if (ggufFiles.length > 0) {
      const q4km = ggufFiles.find(
        (f: HubFileInfo) => detectQuant(f.path)?.label === 'Q4_K_M',
      )
      const target =
        q4km ??
        ggufFiles.reduce((min: HubFileInfo, f: HubFileInfo) => (f.size < min.size ? f : min))
      setSelectedPaths(new Set([target.path]))
      addToast(tr('toast.hubSmartSelect', { path: target.path }), 'success')
      return
    }
    const stFiles = modelFiles.filter((f: HubFileInfo) => f.path.endsWith('.safetensors'))
    if (stFiles.length > 0) {
      setSelectedPaths(new Set(stFiles.map((f: HubFileInfo) => f.path)))
      // 检查是否有被跳过的 .bin/.pt/.pth 文件(走 xet CDN 慢速)
      const skippedBin = modelFiles.filter(
        (f: HubFileInfo) =>
          !f.path.endsWith('.safetensors') && detectFormat(f.path) === 'pytorch',
      )
      if (skippedBin.length > 0) {
        addToast(
          tr('toast.hubSelectedSafetensorsSkippedBin', { count: stFiles.length, skipped: skippedBin.length }),
          'success',
        )
      } else {
        addToast(tr('toast.hubSelectedSafetensors', { count: stFiles.length }), 'success')
      }
      return
    }
    // 无 safetensors 但有 .bin/.pt/.pth:提示用户这些文件可能走 xet CDN 慢速
    const pytorchFiles = modelFiles.filter(
      (f: HubFileInfo) => detectFormat(f.path) === 'pytorch',
    )
    if (pytorchFiles.length > 0) {
      setSelectedPaths(new Set(pytorchFiles.map((f: HubFileInfo) => f.path)))
      addToast(tr('toast.hubSelectedPytorchNoSafetensors', { count: pytorchFiles.length }), 'warning')
      return
    }
  }

  const selectedFiles = createMemo(() =>
    (repoFiles() ?? []).filter((f: HubFileInfo) => selectedPaths().has(f.path)),
  )
  const selectedSize = createMemo(() =>
    (selectedFiles() ?? []).reduce((s: number, f: HubFileInfo) => s + f.size, 0),
  )

  /**
   * 批量下载选中文件。
   * useMirror=true 时用 hf-mirror.com 作为单源下载(基于 repoId 构造,绕过 CDN 域名差异)。
   * 后端默认按 HubConfig.source_mode 处理源(镜像/竞速),此处仅保留显式镜像覆盖入口。
   */
  const handleBatchDownload = async (useMirror: boolean) => {
    const paths = Array.from(selectedPaths())
    if (paths.length === 0) {
      addToast(tr('toast.hubSelectFilesFirst'), 'error')
      return
    }
    const id = repoId().trim()
    const rev = revision().trim() || 'main'
    setBatchDownloading(true)
    try {
      const results = await Promise.allSettled(
        paths.map(async (path) => {
          const originalUrl = await api.getHfDownloadUrl(id, path, rev)
          if (!originalUrl) throw new Error(tr('toast.hubUrlMissing', { path }))
          if (useMirror) {
            // 镜像主源:基于 repoId 构造 hf-mirror resolve URL(鲁棒,绕过 CDN 域名)
            const mirrorUrl = buildHfMirrorUrl(id, rev, path)
            return api.createTask(mirrorUrl)
          }
          return api.createTask(originalUrl)
        }),
      )
      const failed = results.filter((r) => r.status === 'rejected')
      if (failed.length === 0) {
        addToast(
          useMirror ? tr('toast.hubMirrorCreated', { count: paths.length }) : tr('toast.hubCreated', { count: paths.length }),
          'success',
        )
      } else if (failed.length === paths.length) {
        addToast(tr('toast.hubCreateFailed'), 'error')
      } else {
        addToast(tr('toast.batchPartialShort', { success: paths.length - failed.length, failed: failed.length }), 'info')
      }
      refreshTaskList()
      props.onClose()
    } finally {
      setBatchDownloading(false)
    }
  }

  // DI-2:tree memo 化,仅 repoFiles 变化时重建(勾选/筛选不触发树重算)
  const tree = createMemo(() => buildTree(repoFiles() ?? []))
  const fileCount = () => (repoFiles() ?? []).filter((f: HubFileInfo) => f.type !== 'directory').length

  // DI-5:筛选 + 类型计数
  const counts = createMemo(() => countByType(repoFiles() ?? []))
  // 搜索 debounce 150ms(实时筛选响应)
  const [search, setSearch] = createSignal('')
  let searchTimer: ReturnType<typeof setTimeout> | undefined
  const onSearchInput = (v: string) => {
    setSearchInput(v)
    if (searchTimer) clearTimeout(searchTimer)
    searchTimer = setTimeout(() => setSearch(v.trim().toLowerCase()), 150)
  }
  const matchedPaths = createMemo(() => {
    const q = search()
    const f = filter()
    if (f === 'all' && !q) return null // null 表示无筛选,全部匹配
    const set = new Set<string>()
    for (const file of (repoFiles() ?? [])) {
      if (matchesFilter(file, f, q)) set.add(file.path)
    }
    return set
  })

  const filterTabs: { key: FilterKey; label: () => string; count: () => number }[] = [
    { key: 'all', label: () => tr('hub.filter.all'), count: () => counts().all },
    { key: 'gguf', label: () => 'GGUF', count: () => counts().gguf },
    { key: 'safetensors', label: () => 'Safetensors', count: () => counts().safetensors },
    { key: 'large', label: () => tr('hub.filter.large'), count: () => counts().large },
  ]

  return (
    <Show when={props.visible}>
      {/* Overlay */}
      <div class="panel-overlay" style={{ opacity: 1, transition: 'opacity 250ms ease' }} onClick={() => props.onClose()} />

      {/* Panel(Iteration 06 DI-4:移除玻璃拟态,实色 + token 化) */}
      <div
        ref={panelRef}
        class="fixed z-[var(--z-panel-content)] flex flex-col hf-panel"
        role="dialog"
        aria-modal="true"
        aria-label={tr("hub.aria")}
        style={{
          top: '50%',
          left: '50%',
          transform: 'translate(-50%, -50%)',
        }}
      >
        {/* Header */}
        <div class="panel-header">
          <span style={{ 'font-size': '15px', 'font-weight': 600, color: 'var(--color-text-title)' }}>
            HuggingFace Hub
          </span>
          <Button variant="ghost" shape="icon-sm" class="hover-light" aria-label={tr("hub.aria.close")} onClick={() => props.onClose()}>
            <CloseIcon />
          </Button>
        </div>

        {/* 搜索栏 */}
        <div class="flex items-center gap-2" style={{ padding: '12px 20px', 'border-bottom': '1px solid var(--color-border-subtle)' }}>
          <div class="flex items-center gap-2 flex-1" style={{ background: 'var(--graphite-1)', 'border-radius': '8px', padding: '4px 12px' }}>
            <SearchIcon />
            <input
              ref={inputRef}
              type="text"
              class="flex-1"
              placeholder={tr("hub.placeholder.repoId")}
              value={repoId()}
              onInput={(e) => setRepoId(e.currentTarget.value)}
              onKeyDown={(e) => e.key === 'Enter' && handleBrowse()}
              style={{
                'font-size': '13px',
                background: 'transparent',
                color: 'var(--color-text-primary)',
                border: 'none',
                outline: 'none',
              }}
            />
          </div>
          <input
            type="text"
            placeholder={tr("hub.placeholder.revision")}
            value={revision()}
            onInput={(e) => setRevision(e.currentTarget.value)}
            style={{
              width: '140px',
              'font-size': '13px',
              background: 'var(--graphite-1)',
              color: 'var(--color-text-primary)',
              'border-radius': '8px',
              padding: '6px 12px',
              border: 'none',
              outline: 'none',
            }}
          />
          <Button variant="primary" size="sm" loading={loading()} onClick={handleBrowse}>
            {tr("hub.browse")}
          </Button>
        </div>

        {/* 内容区 */}
        <div class="flex-1 scroll-container" style={{ padding: '8px 12px' }}>
          {/* 空状态 */}
          <Show when={!browsed() && !loading() && !error()}>
            <EmptyState
              icon={<HubIcon size={48} />}
              title={tr("hub.empty")}
              description={tr("hub.emptyHint")}
            />
          </Show>

          {/* 加载状态 */}
          <Show when={loading()}>
            <div class="flex flex-col gap-2" style={{ padding: '12px' }}>
              <For each={[0, 1, 2, 3, 4, 5]}>
                {() => <div class="hf-skeleton" />}
              </For>
            </div>
          </Show>

          {/* 错误状态 */}
          <Show when={!loading() && error()}>
            <ErrorState
              compact
              title={tr("hub.loadFailed")}
              detail={error()}
              onRetry={handleRetry}
              retryLabel={tr("hub.retry")}
            />
          </Show>

          {/* 文件树 */}
          <Show when={!loading() && !error() && browsed() && (repoFiles() ?? []).length > 0}>
            {/* DI-5:筛选条 + 类型计数(radiogroup 语义) */}
            <div role="radiogroup" aria-label={tr("hub.aria.filterType")} class="flex items-center gap-1 flex-wrap" style={{ 'margin-bottom': '8px', padding: '4px 8px' }}>
              <For each={filterTabs}>
                {(tab) => (
                  <button
                    type="button"
                    role="radio"
                    aria-checked={filter() === tab.key}
                    class="hf-filter-tab"
                    classList={{ 'hf-filter-tab-active': filter() === tab.key }}
                    disabled={tab.key !== 'all' && tab.count() === 0}
                    onClick={() => setFilter(tab.key)}
                  >
                    {tab.label()} <span class="hf-filter-count">{tab.count()}</span>
                  </button>
                )}
              </For>
              <div class="flex items-center gap-1 flex-1" style={{ 'min-width': '120px', 'margin-left': '8px' }}>
                <SearchIcon />
                <input
                  type="text"
                  placeholder={tr("hub.searchPlaceholder")}
                  value={searchInput()}
                  onInput={(e) => onSearchInput(e.currentTarget.value)}
                  class="hf-filter-search"
                  aria-label={tr("hub.aria.searchFiles")}
                />
              </div>
            </div>

            <div class="flex items-center" style={{ 'margin-bottom': '8px', padding: '4px 8px' }}>
              <span style={{ 'font-size': '12px', color: 'var(--color-text-tertiary)' }}>
                {tr("hub.fileCount", { repoId: repoId(), count: fileCount() })}
              </span>
              <Button
                variant="ghost"
                size="sm"
                class="ml-auto"
                onClick={smartSelect}
                title={tr("hub.smartSelectTitle")}
              >
                {tr("hub.smartSelect")}
              </Button>
            </div>
            <div role="tree" aria-label={tr("hub.aria.fileTree", { repoId: repoId() })}>
              <For each={tree()}>
                {(node) => (
                  <TreeNodeItem
                    node={node}
                    repoId={repoId()}
                    revision={revision()}
                    onDownload={() => {}}
                    depth={0}
                    isSelected={(path) => selectedPaths().has(path)}
                    onToggleSelect={toggleSelect}
                    isMatched={() => {
                      const matched = matchedPaths()
                      if (matched === null) return true
                      return node.isDirectory
                        ? hasMatchedDescendant(node, matched)
                        : matched.has(node.path)
                    }}
                  />
                )}
              </For>
            </div>
          </Show>

          {/* 空仓库 */}
          <Show when={!loading() && !error() && browsed() && (repoFiles() ?? []).length === 0}>
            <EmptyState
              compact
              icon={<FileIcon size={48} />}
              title={tr("hub.emptyRepo")}
            />
          </Show>
        </div>

        {/* 批量下载操作栏 */}
        <Show when={selectedPaths().size > 0}>
          <div
            class="flex items-center gap-2 flex-shrink-0"
            style={{
              padding: '12px 20px',
              'border-top': '1px solid var(--color-border-subtle)',
              background: 'var(--color-bg-secondary)',
            }}
          >
            <span style={{ 'font-size': '13px', color: 'var(--color-text-secondary)' }}>
              {tr("hub.selectedSummary", { count: selectedPaths().size, size: formatSize(selectedSize()) })}
            </span>
            <div style={{ 'margin-left': 'auto' }} class="flex items-center gap-2">
              <Button
                variant="secondary"
                size="md"
                loading={batchDownloading()}
                onClick={() => handleBatchDownload(false)}
                title={tr("hub.downloadTitle")}
              >
                <ArrowDownIcon />
                <span>{tr("hub.download")}</span>
              </Button>
              <Button
                variant="primary"
                size="md"
                loading={batchDownloading()}
                onClick={() => handleBatchDownload(true)}
                title={tr("hub.mirrorDownloadTitle")}
              >
                <ArrowDownIcon />
                <span>{tr("hub.mirrorDownload")}</span>
              </Button>
            </div>
          </div>
        </Show>
      </div>
    </Show>
  )
}

/**
 * 递归判断目录是否有任一后代文件匹配筛选。
 * 用于筛选时决定目录节点是否保持高亮(目录本身非文件,不直接匹配)。
 */
function hasMatchedDescendant(node: TreeNode, matched: Set<string> | null = null): boolean {
  if (matched === null) return true
  for (const child of node.children) {
    if (child.isDirectory) {
      if (hasMatchedDescendant(child, matched)) return true
    } else if (matched.has(child.path)) {
      return true
    }
  }
  return false
}
