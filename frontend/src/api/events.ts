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

/** 启动恢复时检测到损坏快照的告警 payload */
export interface RecoveryWarningPayload {
  corruptKeys: string[]
  count: number
}

/** 监听一次性恢复告警事件(损坏的断点续传快照) */
export async function onRecoveryWarning(
  handler: (payload: RecoveryWarningPayload) => void,
): Promise<UnlistenFn> {
  try {
    const { listen } = await import('@tauri-apps/api/event')
    const unlisten = await listen<RecoveryWarningPayload>('recovery-warning', (e) => handler(e.payload))
    return unlisten
  } catch {
    return () => {}
  }
}
