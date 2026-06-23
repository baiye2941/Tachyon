import { createSignal } from 'solid-js'
import type { HubFileInfo, HfModelInfo } from '../types'
import { api } from '../api/invoke'
import { addToast } from './toast'
import { tr } from '../i18n'

const [repoFiles, setRepoFiles] = createSignal<HubFileInfo[]>([])
const [loading, setLoading] = createSignal(false)
const [error, setError] = createSignal<string | null>(null)

export const $hub = {
  get repoFiles() { return repoFiles },
  get loading() { return loading },
  get error() { return error },
}

export async function listRepoFiles(repoId: string, revision?: string) {
  setLoading(true)
  setError(null)
  try {
    const files = (await api.listRepoFiles(repoId, revision)) ?? []
    setRepoFiles(files)
    return files
  } catch (e) {
    const msg = String(e)
    setError(msg)
    addToast(tr('toast.hubListFailed', { error: msg }), 'error')
    return []
  } finally {
    setLoading(false)
  }
}

export function clearRepoFiles() {
  setRepoFiles([])
  setError(null)
}

export async function getModelInfo(repoId: string, revision?: string): Promise<HfModelInfo | null> {
  try {
    return await api.getModelInfo(repoId, revision)
  } catch {
    return null
  }
}

export async function getHfDownloadUrl(repoId: string, path: string, revision?: string): Promise<string | null> {
  try {
    return await api.getHfDownloadUrl(repoId, path, revision)
  } catch (e) {
    addToast(tr('toast.hubUrlFailed', { error: String(e) }), 'error')
    return null
  }
}
