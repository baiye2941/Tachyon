import { createSignal, Show, For, createMemo, untrack } from 'solid-js'
import { api } from '../api/invoke'
import { $hub, listRepoFiles, clearRepoFiles } from '../stores/hub'
import { addToast } from '../stores/toast'
import { refreshTaskList } from '../stores/downloads'
import type { HubFileInfo } from '../types'
import { CloseIcon, SearchIcon, CheckboxIcon, ArrowDownIcon, ChevronDownIcon, FileIcon } from './icons'
import { detectQuant, isModelWeight, isLargeFile, type QuantLevel } from '../utils/modelMeta'
import { buildTree, countByType, type TreeNode } from '../utils/hfTree'
import { buildHfMirrorUrl } from '../utils/hfMirror'
import Button from '../shared/ui/Button'

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
  isSelected: () => boolean
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
        addToast(`已添加下载: ${props.node.name}`, 'success')
      }
    } catch (e) {
      addToast(`下载失败: ${String(e)}`, 'error')
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
        aria-selected={props.isSelected()}
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
              color: props.isSelected() ? 'var(--color-accent-primary)' : 'var(--color-text-tertiary)',
            }}
            onClick={(e) => {
              e.stopPropagation()
              props.onToggleSelect(props.node.path)
            }}
          >
            <CheckboxIcon checked={props.isSelected()} />
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
            大
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
            aria-label={`下载 ${props.node.name}`}
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
  let inputRef: HTMLInputElement | undefined

  const handleBrowse = async () => {
    const id = repoId().trim()
    if (!id) {
      addToast('请输入仓库 ID', 'error')
      return
    }
    if (!id.includes('/') || id.split('/').length !== 2) {
      addToast('仓库 ID 格式应为 owner/repo', 'error')
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
    const modelFiles = repoFiles().filter(
      (f: HubFileInfo) => f.type !== 'directory' && isModelWeight(f.path),
    )
    if (modelFiles.length === 0) {
      addToast('未发现模型权重文件', 'info')
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
      addToast(`智能选择: ${target.path}`, 'success')
      return
    }
    const stFiles = modelFiles.filter((f: HubFileInfo) => f.path.endsWith('.safetensors'))
    if (stFiles.length > 0) {
      setSelectedPaths(new Set(stFiles.map((f: HubFileInfo) => f.path)))
      addToast(`已选择 ${stFiles.length} 个 safetensors 文件`, 'success')
    }
  }

  const selectedFiles = createMemo(() =>
    repoFiles().filter((f: HubFileInfo) => selectedPaths().has(f.path)),
  )
  const selectedSize = createMemo(() =>
    selectedFiles().reduce((s: number, f: HubFileInfo) => s + f.size, 0),
  )

  /**
   * 批量下载选中文件。
   * useMirror=true 时用 hf-mirror.com 作为主源(基于 repoId 构造,绕过 CDN 域名差异),
   * 原始 HF 链接作为容灾镜像。
   */
  const handleBatchDownload = async (useMirror: boolean) => {
    const paths = Array.from(selectedPaths())
    if (paths.length === 0) {
      addToast('请先勾选要下载的文件', 'error')
      return
    }
    const id = repoId().trim()
    const rev = revision().trim() || 'main'
    setBatchDownloading(true)
    try {
      const results = await Promise.allSettled(
        paths.map(async (path) => {
          const originalUrl = await api.getHfDownloadUrl(id, path, rev)
          if (!originalUrl) throw new Error(`无法获取 ${path} 的下载链接`)
          if (useMirror) {
            // 镜像主源:基于 repoId 构造 hf-mirror resolve URL(鲁棒,绕过 CDN 域名)
            const mirrorUrl = buildHfMirrorUrl(id, rev, path)
            return api.createTask(mirrorUrl, undefined, [originalUrl])
          }
          return api.createTask(originalUrl)
        }),
      )
      const failed = results.filter((r) => r.status === 'rejected')
      if (failed.length === 0) {
        addToast(
          useMirror ? `已通过 hf-mirror 镜像创建 ${paths.length} 个下载任务` : `已创建 ${paths.length} 个下载任务`,
          'success',
        )
      } else if (failed.length === paths.length) {
        addToast('创建下载任务失败', 'error')
      } else {
        addToast(`${paths.length - failed.length} 成功, ${failed.length} 失败`, 'info')
      }
      refreshTaskList()
      props.onClose()
    } finally {
      setBatchDownloading(false)
    }
  }

  const repoFiles = () => $hub.repoFiles()
  const loading = () => $hub.loading()
  const error = () => $hub.error()
  // DI-2:tree memo 化,仅 repoFiles 变化时重建(勾选/筛选不触发树重算)
  const tree = createMemo(() => buildTree(repoFiles()))
  const fileCount = () => repoFiles().filter((f: HubFileInfo) => f.type !== 'directory').length

  // DI-5:筛选 + 类型计数
  const counts = createMemo(() => countByType(repoFiles()))
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
    for (const file of repoFiles()) {
      if (matchesFilter(file, f, q)) set.add(file.path)
    }
    return set
  })

  const filterTabs: { key: FilterKey; label: string; count: () => number }[] = [
    { key: 'all', label: '全部', count: () => counts().all },
    { key: 'gguf', label: 'GGUF', count: () => counts().gguf },
    { key: 'safetensors', label: 'Safetensors', count: () => counts().safetensors },
    { key: 'large', label: '大文件', count: () => counts().large },
  ]

  return (
    <Show when={props.visible}>
      {/* Overlay */}
      <div class="panel-overlay" style={{ opacity: 1, transition: 'opacity 250ms ease' }} onClick={() => props.onClose()} />

      {/* Panel(Iteration 06 DI-4:移除玻璃拟态,实色 + token 化) */}
      <div
        class="fixed z-[210] flex flex-col hf-panel"
        role="dialog"
        aria-modal="true"
        aria-label="HuggingFace Hub"
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
          <Button variant="ghost" shape="icon-sm" class="hover-light" aria-label="关闭" onClick={() => props.onClose()}>
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
              placeholder="owner/repo (如: bert-base-uncased)"
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
            placeholder="revision (默认 main)"
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
            浏览
          </Button>
        </div>

        {/* 内容区 */}
        <div class="flex-1 overflow-y-auto" style={{ padding: '8px 12px' }}>
          {/* 空状态 */}
          <Show when={!browsed() && !loading() && !error()}>
            <div class="flex flex-col items-center justify-center gap-4" style={{ padding: '80px 20px' }}>
              <svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="var(--color-text-tertiary)" stroke-width="1" stroke-linecap="round" stroke-linejoin="round">
                <path d="M18 10h-1.26A8 8 0 1 0 9 20h9a5 5 0 0 0 0-10z" />
              </svg>
              <div style={{ 'font-size': '14px', color: 'var(--color-text-tertiary)' }}>
                输入 HuggingFace 仓库 ID 开始浏览
              </div>
              <div style={{ 'font-size': '12px', color: 'var(--color-text-tertiary)', opacity: 0.6 }}>
                例如: meta-llama/Llama-3.2-1B, openai/clip-vit-base-patch32
              </div>
            </div>
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
            <div class="flex flex-col items-center justify-center gap-3" style={{ padding: '60px 20px' }}>
              <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="var(--color-error)" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
                <circle cx="12" cy="12" r="10" />
                <line x1="12" y1="8" x2="12" y2="12" />
                <line x1="12" y1="16" x2="12.01" y2="16" />
              </svg>
              <div style={{ 'font-size': '14px', color: 'var(--color-error)' }}>
                加载仓库文件列表失败
              </div>
              <div class="mono" style={{ 'font-size': '12px', color: 'var(--color-text-tertiary)' }}>
                {error()}
              </div>
              <Button variant="secondary" size="sm" onClick={handleRetry}>
                重试
              </Button>
            </div>
          </Show>

          {/* 文件树 */}
          <Show when={!loading() && !error() && browsed() && repoFiles().length > 0}>
            {/* DI-5:筛选条 + 类型计数(radiogroup 语义) */}
            <div role="radiogroup" aria-label="文件类型筛选" class="flex items-center gap-1 flex-wrap" style={{ 'margin-bottom': '8px', padding: '4px 8px' }}>
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
                    {tab.label} <span class="hf-filter-count">{tab.count()}</span>
                  </button>
                )}
              </For>
              <div class="flex items-center gap-1 flex-1" style={{ 'min-width': '120px', 'margin-left': '8px' }}>
                <SearchIcon />
                <input
                  type="text"
                  placeholder="搜索文件..."
                  value={searchInput()}
                  onInput={(e) => onSearchInput(e.currentTarget.value)}
                  class="hf-filter-search"
                  aria-label="搜索文件"
                />
              </div>
            </div>

            <div class="flex items-center" style={{ 'margin-bottom': '8px', padding: '4px 8px' }}>
              <span style={{ 'font-size': '12px', color: 'var(--color-text-tertiary)' }}>
                {repoId()} · {fileCount()} 个文件
              </span>
              <Button
                variant="ghost"
                size="sm"
                class="ml-auto"
                onClick={smartSelect}
                title="智能选择最佳量化(GGUF 优先 Q4_K_M)"
              >
                智能选择
              </Button>
            </div>
            <div role="tree" aria-label={`${repoId()} 文件树`}>
              <For each={tree()}>
                {(node) => (
                  <TreeNodeItem
                    node={node}
                    repoId={repoId()}
                    revision={revision()}
                    onDownload={() => {}}
                    depth={0}
                    isSelected={() => selectedPaths().has(node.path)}
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
          <Show when={!loading() && !error() && browsed() && repoFiles().length === 0}>
            <div class="flex flex-col items-center justify-center gap-3" style={{ padding: '60px 20px' }}>
              <div style={{ 'font-size': '14px', color: 'var(--color-text-tertiary)' }}>
                该仓库没有可浏览的文件
              </div>
            </div>
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
              已选 {selectedPaths().size} 个 · {formatSize(selectedSize())}
            </span>
            <div style={{ 'margin-left': 'auto' }} class="flex items-center gap-2">
              <Button
                variant="secondary"
                size="md"
                loading={batchDownloading()}
                onClick={() => handleBatchDownload(false)}
                title="直接从 HuggingFace 下载"
              >
                <ArrowDownIcon />
                <span>下载</span>
              </Button>
              <Button
                variant="primary"
                size="md"
                loading={batchDownloading()}
                onClick={() => handleBatchDownload(true)}
                title="通过 hf-mirror.com 镜像下载(国内加速),原始链接作为容灾"
              >
                <ArrowDownIcon />
                <span>镜像下载</span>
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
