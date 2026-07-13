import { errorMessage } from "../utils/appError";
import { createSignal, Show, For, createMemo, Switch, Match } from 'solid-js'
import type {
  HfModelInfo,
  LocalModel,
  ModelFavorite,
  ModelSourceFilter,
} from '../types'
import { getParentDirectory } from "../utils/path"
import { CloseIcon, FolderOpenIcon, RefreshIcon } from './icons'
import Button from '../shared/ui/Button'
import { tr } from '../i18n'
import { formatSize } from '../utils/format'
import ModelFileTree from './ModelFileTree'
import {
  $model,
  toggleFileSelection,
  clearFileSelection,
  addFavorite,
  removeFavorite,
  verifyModel,
  batchDownload,
} from '../stores/model'
import { addToast } from '../stores/toast'
import { api } from '../api/invoke'

interface ModelDetailProps {
  model: HfModelInfo | LocalModel | ModelFavorite | null
  source: ModelSourceFilter
  onClose: () => void
}

/** 判断是否为 LocalModel */
function isLocalModel(m: unknown): m is LocalModel {
  return m !== null && typeof m === 'object' && 'localPath' in m
}

/** 判断是否为 ModelFavorite */
function isFavoriteModel(m: unknown): m is ModelFavorite {
  return m !== null && typeof m === 'object' && 'addedAt' in m && !('localPath' in m)
}

/** 判断是否为 HfModelInfo */
function isRemoteModel(m: unknown): m is HfModelInfo {
  return m !== null && typeof m === 'object' && 'downloads' in m
}

/** 从各种 model 类型中提取 repo_id */
function getRepoId(model: unknown): string | null {
  if (!model || typeof model !== 'object') return null
  if ('repoId' in model) return (model as { repoId: string }).repoId
  if ('id' in model) return (model as { id: string }).id
  return null
}

/** 提取展示用的文件列表 */
function extractFiles(model: unknown) {
  if (!model || typeof model !== 'object') return [] as { type: string; path: string; size: number; lfs: { oid: string; size: number } | null }[]

  // LocalModel: files 是 LocalModelFile[]
  if ('files' in model && !('siblings' in model)) {
    const lm = model as LocalModel
    return lm.files.map((f) => ({
      type: 'file',
      path: f.path,
      size: f.size,
      lfs: f.lfsOid ? { oid: f.lfsOid, size: f.size } : null,
    }))
  }

  // HfModelInfo: siblings 是 HubFileInfo[]
  if ('siblings' in model) {
    return ((model as HfModelInfo).siblings ?? []).map((f) => ({
      type: f.type,
      path: f.path,
      size: f.size,
      lfs: f.lfs,
    }))
  }

  // ModelFavorite: cached_info.siblings
  if ('cachedInfo' in model && (model as ModelFavorite).cachedInfo) {
    const info = (model as ModelFavorite).cachedInfo
    if (info) {
      return (info.siblings ?? []).map((f) => ({
        type: f.type,
        path: f.path,
        size: f.size,
        lfs: f.lfs,
      }))
    }
  }

  return [] as { type: string; path: string; size: number; lfs: { oid: string; size: number } | null }[]
}

/** 提取模型元数据(用于 remote 和 favorite 的回退) */
function getModelMeta(model: unknown): Partial<HfModelInfo> | null {
  if (!model || typeof model !== 'object') return null
  if (isRemoteModel(model)) return model
  if (isFavoriteModel(model)) return model.cachedInfo ?? null
  if (isLocalModel(model) && model.metadata) return model.metadata
  return null
}

export default function ModelDetail(props: ModelDetailProps) {
  const [verifying, setVerifying] = createSignal(false)
  const [favoriting, setFavoriting] = createSignal(false)
  const [downloading, setDownloading] = createSignal(false)

  const repoId = () => getRepoId(props.model)
  const files = () => extractFiles(props.model)
  const meta = () => getModelMeta(props.model)
  const isLocal = () => isLocalModel(props.model)
  const isRemote = () => isRemoteModel(props.model)
  const isFav = () => isFavoriteModel(props.model)

  // 校验结果(仅 local 模式使用)
  const verifyResults = () => (isLocal() ? $model.verifyResults() : undefined)
  const selectedPaths = () => $model.selectedFilePaths()

  // 选中的文件大小合计
  const selectedSize = createMemo(() => {
    const selected = Array.from(selectedPaths())
    const fileList = files()
    let total = 0
    for (const f of fileList) {
      if (selected.includes(f.path)) {
        total += f.size ?? f.lfs?.size ?? 0
      }
    }
    return total
  })

  const selectedCount = createMemo(() => selectedPaths().size)

  // 格式化日期
  const formatDate = (dateStr: string) => {
    try {
      const d = new Date(dateStr)
      return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
    } catch {
      return dateStr
    }
  }

  // 操作回调
  const handleVerify = async () => {
    const id = repoId()
    if (!id) return
    setVerifying(true)
    try {
      await verifyModel(id)
      addToast(tr('hub.verify.verified'), 'success')
    } finally {
      setVerifying(false)
    }
  }

  const handleRedownload = async () => {
    const id = repoId()
    if (!id) return
    try {
      const paths = Array.from(selectedPaths())
      if (paths.length === 0) {
        addToast(tr('toast.hubSelectFilesFirst'), 'error')
        return
      }
      await batchDownload(id, paths)
      addToast(tr('hub.batch.created', { count: paths.length }), 'success')
    } catch (e) {
      addToast(tr('hub.batch.failed', { error: errorMessage(e) }), 'error')
    }
  }

  const handleOpenDir = async () => {
    if (isLocalModel(props.model)) {
      try {
        // 本地模型路径来自已完成任务 save_path 的父目录,经后端校验在下载根目录内(P1-21)
        await api.openFolderUnderRoot(getParentDirectory((props.model as LocalModel).localPath))
      } catch {
        addToast(tr('toast.openFolderFailed'), 'error')
      }
    }
  }

  const handleToggleFavorite = async () => {
    const id = repoId()
    if (!id) return
    setFavoriting(true)
    try {
      if (isFav()) {
        await removeFavorite(id)
      } else {
        await addFavorite(id)
      }
    } finally {
      setFavoriting(false)
    }
  }

  const handleDownloadSelected = async () => {
    const id = repoId()
    if (!id) return
    const paths = Array.from(selectedPaths())
    if (paths.length === 0) {
      addToast(tr('toast.hubSelectFilesFirst'), 'error')
      return
    }
    setDownloading(true)
    try {
      await batchDownload(id, paths)
      addToast(tr('hub.batch.created', { count: paths.length }), 'success')
      clearFileSelection()
    } catch (e) {
      addToast(tr('hub.batch.failed', { error: errorMessage(e) }), 'error')
    } finally {
      setDownloading(false)
    }
  }

  return (
    <div
      class="flex flex-col h-full"
      style={{
        background: 'var(--color-bg-secondary)',
        'border-left': '1px solid var(--color-border-subtle)',
      }}
    >
      {/* Header */}
      <div
        class="flex items-center justify-between flex-shrink-0"
        style={{
          padding: '12px 16px',
          'border-bottom': '1px solid var(--color-border-subtle)',
        }}
      >
        <div
          class="truncate"
          style={{
            'font-size': '15px',
            'font-weight': 600,
            color: 'var(--color-text-title)',
            'max-width': 'calc(100% - 40px)',
          }}
        >
          {repoId() ?? tr('common.unknown')}
        </div>
        <Button
          variant="ghost"
          shape="icon-sm"
          aria-label={tr('detail.closeAria')}
          onClick={props.onClose}
        >
          <CloseIcon />
        </Button>
      </div>

      {/* Content */}
      <div class="flex-1 scroll-container" style={{ padding: '16px' }}>
        {/* Empty state */}
        <Show when={!props.model}>
          <div
            class="flex flex-col items-center justify-center"
            style={{ padding: '60px 20px', 'min-height': '200px' }}
          >
            <div
              style={{
                'font-size': '14px',
                color: 'var(--color-text-tertiary)',
                'text-align': 'center',
              }}
            >
              {tr('hub.search.noResult')}
            </div>
          </div>
        </Show>

        <Show when={props.model}>
          {/* Metadata section */}
          <div style={{ 'margin-bottom': '20px' }}>
            {/* Author */}
            <Show when={meta()?.author}>
              <div
                style={{
                  'font-size': '13px',
                  color: 'var(--color-text-secondary)',
                  'margin-bottom': '8px',
                }}
              >
                {tr('hub.detail.author', { author: meta()!.author! })}
              </div>
            </Show>

            {/* Description */}
            <Show when={meta()?.cardData?.description}>
              <div
                style={{
                  'font-size': '12px',
                  color: 'var(--color-text-secondary)',
                  'line-height': '1.5',
                  'margin-bottom': '12px',
                }}
              >
                {meta()!.cardData!.description}
              </div>
            </Show>

            {/* Tags */}
            <Show when={meta()?.tags && meta()!.tags!.length > 0}>
              <div
                class="flex flex-wrap"
                style={{ gap: '4px', 'margin-bottom': '12px' }}
              >
                <For each={meta()!.tags!.slice(0, 10)}>
                  {(tag) => (
                    <span
                      style={{
                        'font-size': '11px',
                        padding: '2px 8px',
                        'border-radius': '4px',
                        background: 'var(--color-bg-elevated)',
                        color: 'var(--color-text-secondary)',
                        border: '1px solid var(--color-border-subtle)',
                      }}
                    >
                      {tag}
                    </span>
                  )}
                </For>
              </div>
            </Show>

            {/* Meta grid */}
            <div
              style={{
                display: 'grid',
                'grid-template-columns': '1fr 1fr',
                gap: '8px',
                'font-size': '12px',
                color: 'var(--color-text-secondary)',
              }}
            >
              <Show when={meta()?.libraryName}>
                <span>
                  {tr('hub.detail.framework', {
                    name: meta()!.libraryName!,
                  })}
                </span>
              </Show>
              <Show when={meta()?.license}>
                <span>
                  {tr('hub.detail.license', { name: meta()!.license! })}
                </span>
              </Show>
              <Show when={meta()?.downloads !== undefined}>
                <span>
                  {tr('hub.detail.downloads', {
                    count: String(meta()!.downloads),
                  })}
                </span>
              </Show>
              <Show when={meta()?.likes !== undefined}>
                <span>
                  {tr('hub.detail.likes', { count: String(meta()!.likes) })}
                </span>
              </Show>
              <Show when={meta()?.lastModified}>
                <span>
                  {tr('hub.detail.updated', {
                    date: formatDate(meta()!.lastModified!),
                  })}
                </span>
              </Show>
            </div>
          </div>

          {/* File tree section */}
          <div style={{ 'margin-bottom': '20px' }}>
            <div
              style={{
                'font-size': '14px',
                'font-weight': 600,
                color: 'var(--color-text-title)',
                'margin-bottom': '12px',
              }}
            >
              {tr('hub.fileTree.modelWeight')}
            </div>
            <ModelFileTree
              files={files()}
              selectedPaths={selectedPaths()}
              verifyResults={verifyResults()}
              onToggleSelection={toggleFileSelection}
              readOnly={isLocal()}
            />
          </div>
        </Show>
      </div>

      {/* Bottom action area */}
      <Show when={props.model}>
        {/* Selected files summary */}
        <Show when={selectedCount() > 0}>
          <div
            class="flex items-center justify-between flex-shrink-0"
            style={{
              padding: '10px 16px',
              'border-top': '1px solid var(--color-border-subtle)',
              background: 'var(--color-bg-elevated)',
              'font-size': '13px',
              color: 'var(--color-text-secondary)',
            }}
          >
            <span>
              {tr('hub.fileTree.selected', {
                count: selectedCount(),
                size: formatSize(selectedSize()),
              })}
            </span>
            <Button
              variant="primary"
              size="sm"
              loading={downloading()}
              onClick={handleDownloadSelected}
            >
              {tr('hub.action.downloadSelected')}
            </Button>
          </div>
        </Show>

        {/* Action buttons */}
        <div
          class="flex items-center gap-2 flex-shrink-0 flex-wrap"
          style={{
            padding: '12px 16px',
            'border-top': '1px solid var(--color-border-subtle)',
            background: 'var(--color-bg-secondary)',
          }}
        >
          <Switch>
            {/* Local model actions */}
            <Match when={isLocal()}>
              <Button
                variant="secondary"
                size="sm"
                loading={verifying()}
                onClick={handleVerify}
              >
                <RefreshIcon />
                {tr('hub.action.verify')}
              </Button>
              <Button variant="secondary" size="sm" onClick={handleRedownload}>
                <RefreshIcon />
                {tr('hub.action.redownload')}
              </Button>
              <Button variant="secondary" size="sm" onClick={handleOpenDir}>
                <FolderOpenIcon />
                {tr('hub.action.openDir')}
              </Button>
            </Match>

            {/* Remote model actions */}
            <Match when={isRemote()}>
              <Button
                variant="primary"
                size="sm"
                loading={downloading()}
                onClick={handleDownloadSelected}
              >
                <RefreshIcon />
                {tr('hub.action.downloadSelected')}
              </Button>
              <Button
                variant="secondary"
                size="sm"
                loading={favoriting()}
                onClick={handleToggleFavorite}
              >
                {isFav() ? tr('hub.action.unfavorite') : tr('hub.action.favorite')}
              </Button>
            </Match>

            {/* Favorite model actions */}
            <Match when={isFav()}>
              <Button
                variant="secondary"
                size="sm"
                loading={favoriting()}
                onClick={handleToggleFavorite}
              >
                {tr('hub.action.unfavorite')}
              </Button>
              <Button
                variant="primary"
                size="sm"
                loading={downloading()}
                onClick={handleDownloadSelected}
              >
                <RefreshIcon />
                {tr('hub.action.downloadSelected')}
              </Button>
            </Match>
          </Switch>
        </div>
      </Show>
    </div>
  )
}
