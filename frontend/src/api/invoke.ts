import type { TaskInfo, AppConfig, SnifferResource } from '../types'

async function getInvoke(): Promise<typeof import('@tauri-apps/api/core').invoke> {
  try {
    const mod = await import('@tauri-apps/api/core')
    return mod.invoke
  } catch {
    throw new Error(
      'Tauri API 不可用 -- 请通过 `cargo tauri dev` 启动应用,不要直接在浏览器打开',
    )
  }
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const fn = await getInvoke()
  return fn(cmd, args) as Promise<T>
}

export const api = {
  createTask: (url: string, mirrorUrls?: string[]) => invoke<string>('create_task', { url, mirrorUrls }),
  getTaskList: () => invoke<TaskInfo[]>('get_task_list'),
  getTaskDetail: (taskId: string) => invoke<TaskInfo>('get_task_detail', { taskId }),
  pauseTask: (taskId: string) => invoke<void>('pause_task', { taskId }),
  resumeTask: (taskId: string) => invoke<void>('resume_task', { taskId }),
  cancelTask: (taskId: string) => invoke<void>('cancel_task', { taskId }),
  deleteTask: (taskId: string) => invoke<void>('delete_task', { taskId }),
  getConfig: () => invoke<AppConfig>('get_config'),
  updateConfig: (config: Partial<AppConfig>) => invoke<void>('update_config', { config }),
  getSnifferResources: () => invoke<SnifferResource[]>('get_sniffer_resources'),
  addSnifferFilter: (filter: string) => invoke<void>('add_sniffer_filter', { filter }),
  subscribeProgress: () => invoke<void>('subscribe_progress'),
}
