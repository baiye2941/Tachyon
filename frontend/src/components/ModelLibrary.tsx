import {
  createSignal,
  For,
  Show,
  createMemo,
  createEffect,
} from 'solid-js'
import type {
  HfModelInfo,
  LocalModel,
  ModelFavorite,
  ModelSourceFilter,
} from '../types'
import {
  $model,
  scanLocalModels,
  searchRemoteModels,
  loadFavorites,
  clearFileSelection,
} from '../stores/model'
import { tr } from '../i18n'
import { formatSize } from '../utils/format'
import { SearchIcon } from './icons'
import Button from '../shared/ui/Button'
import ModelDetail from './ModelDetail'

interface ModelCard {
  id: string
  repoId: string
  framework?: string
  size: number
  isDownloaded: boolean
  isFavorite: boolean
  meta: Partial<HfModelInfo> | null
}

/** 构建统一卡片数据 */
function buildModelCard(
  model: HfModelInfo | LocalModel | ModelFavorite,
  allFavorites: ModelFavorite[],
): ModelCard {
  if ('localPath' in model) {
    // LocalModel
    return {
      id: model.repoId,
      repoId: model.repoId,
      framework: model.metadata?.libraryName,
      size: model.totalSize,
      isDownloaded: true,
      isFavorite: allFavorites.some((f) => f.repoId === model.repoId),
      meta: model.metadata ?? null,
    }
  }
  if ('addedAt' in model) {
    // ModelFavorite
    const info = model.cachedInfo
    return {
      id: model.repoId,
      repoId: model.repoId,
      framework: info?.libraryName,
      size: 0,
      isDownloaded: false,
      isFavorite: true,
      meta: info ?? null,
    }
  }
  // HfModelInfo
  return {
    id: model.id,
    repoId: model.id,
    framework: model.libraryName,
    size: 0,
    isDownloaded: false,
    isFavorite: allFavorites.some((f) => f.repoId === model.id),
    meta: model,
  }
}

export default function ModelLibrary() {
  // Local state
  const [searchInput, setSearchInput] = createSignal('')
  const [debouncedQuery, setDebouncedQuery] = createSignal('')
  let searchTimer: ReturnType<typeof setTimeout> | undefined
  const [selectedModelId, setSelectedModelId] = createSignal<string | null>(null)

  // Reactive store access
  const sourceFilter = () => $model.sourceFilter()
  const localModels = () => $model.localModels()
  const remoteModels = () => $model.remoteModels()
  const favorites = () => $model.favorites()
  const scanning = () => $model.scanning()
  const searching = () => $model.searching()

  // Derived: filtered cards based on source
  const cards = createMemo<ModelCard[]>(() => {
    const q = debouncedQuery().toLowerCase().trim()
    let result: ModelCard[] = []

    if (sourceFilter() === 'local') {
      result = localModels().map((m) => buildModelCard(m, favorites()))
    } else if (sourceFilter() === 'remote') {
      result = remoteModels().map((m) => buildModelCard(m, favorites()))
    } else {
      result = favorites().map((m) => buildModelCard(m, favorites()))
    }

    if (q) {
      result = result.filter(
        (c) =>
          c.repoId.toLowerCase().includes(q) ||
          c.framework?.toLowerCase().includes(q),
      )
    }
    return result
  })

  // Derived: total stats
  const totalCount = () => cards().length
  const totalSize = createMemo(() =>
    cards().reduce((s, c) => s + c.size, 0),
  )

  // Selected model object
  const selectedModel = createMemo(() => {
    const id = selectedModelId()
    if (!id) return null
    if (sourceFilter() === 'local') {
      return localModels().find((m) => m.repoId === id) ?? null
    }
    if (sourceFilter() === 'remote') {
      return remoteModels().find((m) => m.id === id) ?? null
    }
    return favorites().find((m) => m.repoId === id) ?? null
  })

  // Auto-load data when source changes
  createEffect(() => {
    const source = sourceFilter()
    clearFileSelection()
    setSelectedModelId(null)

    if (source === 'local') {
      void scanLocalModels()
    } else if (source === 'remote') {
      void searchRemoteModels('')
    } else {
      void loadFavorites()
    }
  })

  // Search debounce
  const onSearchInput = (value: string) => {
    setSearchInput(value)
    if (searchTimer) clearTimeout(searchTimer)
    searchTimer = setTimeout(() => setDebouncedQuery(value.trim()), 300)
  }

  const handleSearch = () => {
    if (sourceFilter() === 'remote') {
      void searchRemoteModels(debouncedQuery() || '')
    }
  }

  // Source filter tabs
  const sourceTabs: { key: ModelSourceFilter; label: string }[] = [
    { key: 'local', label: tr('hub.tab.local') },
    { key: 'remote', label: tr('hub.tab.remote') },
    { key: 'favorite', label: tr('hub.tab.favorite') },
  ]

  return (
    <div class="flex flex-col h-full" style={{ overflow: 'hidden' }}>
      {/* Top bar: search + source filters */}
      <div
        class="flex items-center gap-3 flex-shrink-0"
        style={{
          padding: '12px 16px',
          'border-bottom': '1px solid var(--color-border-subtle)',
          background: 'var(--color-bg-secondary)',
        }}
      >
        {/* Source filter tabs */}
        <div class="flex items-center gap-1">
          <For each={sourceTabs}>
            {(tab) => (
              <Button
                variant={sourceFilter() === tab.key ? 'primary' : 'secondary'}
                size="sm"
                onClick={() => $model.setSourceFilter(tab.key)}
              >
                {tab.label}
              </Button>
            )}
          </For>
        </div>

        {/* Search input */}
        <div
          class="flex items-center gap-2 flex-1"
          style={{
            'max-width': '360px',
            background: 'var(--color-bg-elevated)',
            'border-radius': '8px',
            padding: '4px 12px',
            border: '1px solid var(--color-border-subtle)',
          }}
        >
          <div
            style={{
              color: 'var(--color-text-tertiary)',
              'flex-shrink': 0,
            }}
          >
            <SearchIcon />
          </div>
          <input
            type="text"
            placeholder={tr('hub.search.placeholder')}
            value={searchInput()}
            onInput={(e) => onSearchInput(e.currentTarget.value)}
            onKeyDown={(e) => e.key === 'Enter' && handleSearch()}
            style={{
              flex: 1,
              'font-size': '13px',
              background: 'transparent',
              border: 'none',
              outline: 'none',
              color: 'var(--color-text-primary)',
            }}
          />
        </div>
      </div>

      {/* Main content: left panel (60%) + right panel (40%) */}
      <div class="flex flex-1" style={{ overflow: 'hidden' }}>
        {/* Left panel: model card list */}
        <div
          class="flex flex-col"
          style={{
            width: '60%',
            'min-width': 0,
            'border-right': '1px solid var(--color-border-subtle)',
            overflow: 'hidden',
          }}
        >
          <div class="flex-1 scroll-container" style={{ padding: '12px' }}>
            {/* Loading state */}
            <Show when={scanning() || searching()}>
              <div
                class="flex items-center justify-center"
                style={{
                  padding: '40px',
                  color: 'var(--color-text-tertiary)',
                  'font-size': '14px',
                }}
              >
                {tr('common.loading')}
              </div>
            </Show>

            {/* Empty state */}
            <Show when={!scanning() && !searching() && cards().length === 0}>
              <div
                class="flex flex-col items-center justify-center"
                style={{ padding: '60px 20px' }}
              >
                <div
                  style={{
                    'font-size': '14px',
                    color: 'var(--color-text-tertiary)',
                    'text-align': 'center',
                  }}
                >
                  {sourceFilter() === 'local'
                    ? tr('hub.scan.empty')
                    : sourceFilter() === 'favorite'
                      ? tr('hub.favorite.empty')
                      : tr('hub.search.noResult')}
                </div>
              </div>
            </Show>

            {/* Model cards */}
            <For each={cards()}>
              {(card) => (
                <div
                  class="cursor-pointer"
                  onClick={() => setSelectedModelId(card.id)}
                  style={{
                    padding: '12px 16px',
                    'border-radius': '8px',
                    background:
                      selectedModelId() === card.id
                        ? 'var(--color-bg-elevated)'
                        : 'transparent',
                    border:
                      selectedModelId() === card.id
                        ? '1px solid var(--color-accent-primary)'
                        : '1px solid var(--color-border-subtle)',
                    'margin-bottom': '8px',
                    transition: 'all 150ms ease',
                  }}
                >
                  <div
                    class="flex items-center justify-between"
                    style={{ 'margin-bottom': '6px' }}
                  >
                    <span
                      class="truncate"
                      style={{
                        'font-size': '14px',
                        'font-weight': 600,
                        color: 'var(--color-text-title)',
                        'max-width': '70%',
                      }}
                    >
                      {card.repoId}
                    </span>
                    <div class="flex items-center gap-2">
                      <Show when={card.isFavorite}>
                        <div
                          style={{
                            color: 'var(--color-accent-primary)',
                            'flex-shrink': 0,
                          }}
                        >
                          <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor">
                            <polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2" />
                          </svg>
                        </div>
                      </Show>
                      <Show when={card.isDownloaded}>
                        <span
                          style={{
                            'font-size': '10px',
                            padding: '2px 6px',
                            'border-radius': '4px',
                            background: 'var(--color-status-completed)',
                            color: '#fff',
                          }}
                        >
                          {tr('hub.card.downloaded')}
                        </span>
                      </Show>
                    </div>
                  </div>

                  <div
                    class="flex items-center justify-between"
                    style={{ 'font-size': '12px', color: 'var(--color-text-secondary)' }}
                  >
                    <div class="flex items-center gap-2">
                      <Show when={card.framework}>
                        <span
                          style={{
                            'font-size': '11px',
                            padding: '2px 6px',
                            'border-radius': '4px',
                            background: 'var(--color-bg-elevated)',
                            color: 'var(--color-text-secondary)',
                          }}
                        >
                          {card.framework}
                        </span>
                      </Show>
                      <span class="mono">{formatSize(card.size)}</span>
                    </div>
                    <span>
                      {tr('hub.card.files', {
                        count: card.meta?.siblings?.length ?? 0,
                      })}
                    </span>
                  </div>
                </div>
              )}
            </For>
          </div>

          {/* Stats bar */}
          <div
            class="flex items-center flex-shrink-0"
            style={{
              padding: '8px 16px',
              'border-top': '1px solid var(--color-border-subtle)',
              'font-size': '12px',
              color: 'var(--color-text-tertiary)',
              background: 'var(--color-bg-secondary)',
            }}
          >
            <span>
              {tr("hub.library.summary", {
                count: totalCount(),
                size: formatSize(totalSize()),
              })}
            </span>
          </div>
        </div>

        {/* Right panel: ModelDetail */}
        <div style={{ width: '40%', 'min-width': 0, overflow: 'hidden' }}>
          <ModelDetail
            model={selectedModel()}
            source={sourceFilter()}
            onClose={() => {
              setSelectedModelId(null)
              clearFileSelection()
            }}
          />
        </div>
      </div>
    </div>
  )
}
