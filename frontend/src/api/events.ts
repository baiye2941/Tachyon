import type { ProgressEvent } from '../types'

type UnlistenFn = () => void

export async function onProgressUpdate(handler: (payload: ProgressEvent) => void): Promise<UnlistenFn> {
  if (window.__TAURI__) {
    try {
      const { onProgressUpdate } = await import('../services/downloadChannel')
      await onProgressUpdate(handler)
      const { bindProgressChannel } = await import('../services/downloadChannel')
      bindProgressChannel().catch(() => {})
      return () => {}
    } catch {
      const { listen } = await import('@tauri-apps/api/event')
      const unlisten = await listen<ProgressEvent>('progress-update', (e) => handler(e.payload))
      return unlisten
    }
  }
  return () => {}
}
