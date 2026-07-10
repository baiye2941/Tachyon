import type { ProgressEvent, SnifferResource, ClipboardUrlDetected } from '../types'

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

/** 监听新嗅探资源事件(手动添加或未来 adapter 注入时触发) */
export async function onSnifferResourceAdded(
  handler: (payload: SnifferResource) => void,
): Promise<UnlistenFn> {
  try {
    const { listen } = await import('@tauri-apps/api/event')
    const unlisten = await listen<SnifferResource>('sniffer://resource-added', (e) => handler(e.payload))
    return unlisten
  } catch {
    return () => {}
  }
}

/** 监听剪贴板 URL 检测事件(后端轮询发现可下载 URL 时触发) */
export async function onClipboardUrlDetected(
  handler: (payload: ClipboardUrlDetected) => void,
): Promise<UnlistenFn> {
  try {
    const { listen } = await import('@tauri-apps/api/event')
    const unlisten = await listen<ClipboardUrlDetected>('clipboard://url-detected', (e) => handler(e.payload))
    return unlisten
  } catch {
    return () => {}
  }
}
