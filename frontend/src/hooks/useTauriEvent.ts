import { onCleanup } from 'solid-js'
import { onProgressUpdate } from '../api/events'
import { updateProgress } from '../stores/downloads'

export function useProgressListener() {
  const unlistenPromise = onProgressUpdate((payload) => {
    if (!payload || typeof payload !== 'object') return
    updateProgress(payload)
  })

  onCleanup(() => {
    unlistenPromise.then(fn => fn())
  })
}