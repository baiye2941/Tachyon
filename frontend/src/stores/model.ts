import { createSignal } from 'solid-js'
import type { LocalModel, HfModelInfo, ModelFavorite, ModelSourceFilter, FileVerifyResult } from '../types'
import { api } from '../api/invoke'
import { addToast } from './toast'
import { tr } from '../i18n'
import { isRepoId } from '../utils/hfUrl'

// -- 来源过滤器 --
const [sourceFilter, setSourceFilter] = createSignal<ModelSourceFilter>('local')
// -- 搜索 --
const [searchQuery, setSearchQuery] = createSignal('')
const [searching, setSearching] = createSignal(false)
// -- 本地模型 --
const [localModels, setLocalModels] = createSignal<LocalModel[]>([])
const [scanning, setScanning] = createSignal(false)
// -- 远程搜索结果 --
const [remoteModels, setRemoteModels] = createSignal<HfModelInfo[]>([])
// -- 收藏 --
const [favorites, setFavorites] = createSignal<ModelFavorite[]>([])
// -- 选中的模型 --
const [selectedModelId, setSelectedModelId] = createSignal<string | null>(null)
// -- 选中的文件（用于选择性下载） --
const [selectedFilePaths, setSelectedFilePaths] = createSignal<Set<string>>(new Set())
// -- 校验结果 --
const [verifyResults, setVerifyResults] = createSignal<Record<string, FileVerifyResult>>({})
const [verifying, setVerifying] = createSignal(false)

export const $model = {
  get sourceFilter() { return sourceFilter },
  get searchQuery() { return searchQuery },
  get searching() { return searching },
  get localModels() { return localModels },
  get scanning() { return scanning },
  get remoteModels() { return remoteModels },
  get favorites() { return favorites },
  get selectedModelId() { return selectedModelId },
  get selectedFilePaths() { return selectedFilePaths },
  get verifyResults() { return verifyResults },
  get verifying() { return verifying },
  setSourceFilter,
  setSearchQuery,
  setSelectedModelId,
  setSelectedFilePaths,
  setVerifyResults,
}

// -- 操作 --

export async function scanLocalModels() {
  setScanning(true)
  try {
    const models = await api.scanLocalModels()
    setLocalModels(models)
    return models
  } catch (e) {
    addToast(tr('hub.scan.error', { error: String(e) }), 'error')
    return []
  } finally {
    setScanning(false)
  }
}

export async function searchRemoteModels(query: string) {
  setSearching(true)
  setSearchQuery(query)
  try {
    if (isRepoId(query)) {
      const info = await api.getModelInfo(query)
      setRemoteModels(info ? [info] : [])
    } else {
      const results = await api.searchModels(query)
      setRemoteModels(results)
    }
    return remoteModels()
  } catch (e) {
    addToast(tr('hub.search.error', { error: String(e) }), 'error')
    setRemoteModels([])
    return []
  } finally {
    setSearching(false)
  }
}

export async function loadFavorites() {
  try {
    const favs = await api.listModelFavorites()
    setFavorites(favs)
    return favs
  } catch (e) {
    return []
  }
}

export async function addFavorite(repoId: string) {
  try {
    await api.addModelFavorite(repoId)
    addToast(tr('hub.favorite.added'), 'success')
    await loadFavorites()
  } catch (e) {
    addToast(tr('hub.favorite.addFailed', { error: String(e) }), 'error')
  }
}

export async function removeFavorite(repoId: string) {
  try {
    await api.removeModelFavorite(repoId)
    addToast(tr('hub.favorite.removed'), 'success')
    await loadFavorites()
  } catch (e) {
    addToast(tr('hub.favorite.removeFailed', { error: String(e) }), 'error')
  }
}

export async function verifyModel(repoId: string, revision?: string) {
  setVerifying(true)
  try {
    const results = await api.verifyModel(repoId, revision)
    const map: Record<string, FileVerifyResult> = {}
    for (const r of results) {
      map[r.path] = r
    }
    setVerifyResults(map)
    return results
  } catch (e) {
    return []
  } finally {
    setVerifying(false)
  }
}

export async function batchDownload(repoId: string, filePaths: string[], revision?: string, downloadDir?: string) {
  try {
    const ids = await api.batchCreateHfTasks(repoId, filePaths, revision, downloadDir)
    addToast(tr('hub.batch.created', { count: ids.length }), 'success')
    return ids
  } catch (e) {
    addToast(tr('hub.batch.failed', { error: String(e) }), 'error')
    return []
  }
}

export function toggleFileSelection(filePath: string) {
  setSelectedFilePaths((prev) => {
    const next = new Set(prev)
    if (next.has(filePath)) {
      next.delete(filePath)
    } else {
      next.add(filePath)
    }
    return next
  })
}

export function clearFileSelection() {
  setSelectedFilePaths(new Set<string>())
}
