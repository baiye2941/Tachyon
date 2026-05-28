import type { TaskInfo, AppConfig, SnifferResource } from '../types'

declare global {
  interface Window {
    __TAURI__?: {
      core: {
        invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>
      }
    }
  }
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!window.__TAURI__) {
    throw new Error('Tauri API not available')
  }
  return window.__TAURI__.core.invoke(cmd, args) as Promise<T>
}

export const api = {
  createTask: (url: string) => invoke<string>('create_task', { url }),
  getTaskList: () => invoke<TaskInfo[]>('get_task_list'),
  getTaskDetail: (taskId: string) => invoke<TaskInfo>('get_task_detail', { taskId }),
  pauseTask: (taskId: string) => invoke<void>('pause_task', { taskId }),
  resumeTask: (taskId: string) => invoke<void>('resume_task', { taskId }),
  cancelTask: (taskId: string) => invoke<void>('cancel_task', { taskId }),
  deleteTask: (taskId: string) => invoke<void>('delete_task', { taskId }),
  getConfig: () => invoke<AppConfig>('get_config'),
  updateConfig: (config: Partial<AppConfig>) => invoke<void>('update_config', { config }),
  getSnifferResources: () => invoke<SnifferResource[]>('get_sniffer_resources'),
  subscribeProgress: () => invoke<void>('subscribe_progress'),
}
