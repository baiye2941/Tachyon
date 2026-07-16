import type { ProgressEvent, SnifferResource, ClipboardUrlDetected } from '../types'

type UnlistenFn = () => void

/**
 * 审计 FT-05:listen 注册失败必须 reject,调用方可 toast/重连。
 * 不再把注册失败吞成 no-op unlisten(否则 .catch 分支不可达)。
 */
export async function onProgressUpdate(
  handler: (payload: ProgressEvent) => void,
): Promise<UnlistenFn> {
  const { listen } = await import('@tauri-apps/api/event')
  return listen<ProgressEvent>('progress-update', (e) => handler(e.payload))
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
  const { listen } = await import('@tauri-apps/api/event')
  return listen<RecoveryWarningPayload>('recovery-warning', (e) => handler(e.payload))
}

/** 监听新嗅探资源事件(手动添加或未来 adapter 注入时触发) */
export async function onSnifferResourceAdded(
  handler: (payload: SnifferResource) => void,
): Promise<UnlistenFn> {
  const { listen } = await import('@tauri-apps/api/event')
  return listen<SnifferResource>('sniffer://resource-added', (e) => handler(e.payload))
}

/** 监听剪贴板 URL 检测事件(后端轮询发现可下载 URL 时触发) */
export async function onClipboardUrlDetected(
  handler: (payload: ClipboardUrlDetected) => void,
): Promise<UnlistenFn> {
  const { listen } = await import('@tauri-apps/api/event')
  return listen<ClipboardUrlDetected>('clipboard://url-detected', (e) => handler(e.payload))
}

/** 任务终态通知事件 payload */
export interface TaskNotificationPayload {
  taskId: string
  title: string
  body: string
  type: 'completed' | 'failed'
}

/** 监听任务终态通知事件(Completed/Failed) */
export async function onTaskNotification(
  handler: (payload: TaskNotificationPayload) => void,
): Promise<UnlistenFn> {
  const { listen } = await import('@tauri-apps/api/event')
  return listen<TaskNotificationPayload>('task-notification', (e) => handler(e.payload))
}
