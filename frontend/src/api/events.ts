import type { ProgressEvent } from '../types'

type UnlistenFn = () => void

export async function onProgressUpdate(handler: (payload: ProgressEvent) => void): Promise<UnlistenFn> {
  try {
    const { listen } = await import('@tauri-apps/api/event')
    const unlisten = await listen<ProgressEvent>('progress-update', (e) => handler(e.payload))
    return unlisten
  } catch {
    return () => {}
  }
}
