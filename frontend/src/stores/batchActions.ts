import { api } from '../api/invoke'
import { $tasks, refreshTaskList } from './downloads'
import { $selectedIds, deselectAll } from './selection'
import { addToast } from './toast'
import { requestConfirm } from './confirm'
import { tr } from '../i18n'

export async function pauseSelected(): Promise<void> {
  const ids = Array.from($selectedIds.get())
  if (ids.length === 0) return

  await Promise.allSettled(ids.map(id => api.pauseTask(id)))
  deselectAll()
  await refreshTaskList()
}

export async function resumeSelected(): Promise<void> {
  const ids = Array.from($selectedIds.get())
  if (ids.length === 0) return

  await Promise.allSettled(ids.map(id => api.resumeTask(id)))
  deselectAll()
  await refreshTaskList()
}

/**
 * 批量取消选中任务
 *
 * cancel = 立即停止下载但保留任务记录(区别于 delete)。cancel_task 是 mutate
 * 级(非 destructive),后端无需 confirmation token;但批量操作为防误触,前端
 * 走一次应用内 ConfirmDialog(中性 tone,提示"停止但保留记录")。
 */
export async function cancelSelected(): Promise<void> {
  const ids = Array.from($selectedIds.get())
  if (ids.length === 0) return

  const result = await requestConfirm({
    title: tr('confirm.cancelBatch.title'),
    message: tr('confirm.cancelBatch.message', { count: ids.length }),
    confirmLabel: tr('confirm.cancelBatch.confirmLabel'),
  })
  if (!result.ok) return

  const results = await Promise.allSettled(ids.map(id => api.cancelTask(id)))
  const failures = results.filter(r => r.status === 'rejected') as PromiseRejectedResult[]
  if (failures.length > 0) {
    addToast(tr('toast.cancelBatchPartialFailed', {
      count: failures.length,
      error: failures[0]?.reason ?? '',
    }), 'error')
  }
  deselectAll()
  await refreshTaskList()
}

/**
 * 批量删除选中任务(Iteration 11)
 *
 * 改造前:Tauri plugin-dialog 弹一次确认 + 每个 deleteTask 内部 window.confirm
 *         共 N+1 个原生对话框,严重违背批量操作语义。
 * 改造后:应用内 ConfirmDialog 单次确认(danger tone),后续 deleteTask
 *         传 skipConfirm:true 跳过 invoke 内置 window.confirm。
 *         后端 confirmation token 机制仍对每个删除生效,安全边界不变。
 */
export async function deleteSelected(): Promise<void> {
  const ids = Array.from($selectedIds.get())
  if (ids.length === 0) return

  const result = await requestConfirm({
    title: tr('confirm.deleteBatch.title'),
    message: tr('confirm.deleteBatch.message', { count: ids.length }),
    confirmLabel: tr('confirm.delete.confirmLabel'),
    tone: 'danger',
    showDeleteLocalFileOption: true,
    deleteLocalFileDefault: false,
  })
  if (!result.ok) return

  const results = await Promise.allSettled(ids.map(id => api.deleteTask(id, {
    skipConfirm: true,
    deleteLocalFile: result.deleteLocalFile,
  })))
  const failures = results.filter(r => r.status === 'rejected') as PromiseRejectedResult[]
  if (failures.length > 0) {
    addToast(tr('toast.deleteBatchPartialFailed', {
      count: failures.length,
      error: failures[0]?.reason ?? '',
    }), 'error')
  }
  deselectAll()
  await refreshTaskList()
}

export async function pauseAll(): Promise<void> {
  const ids = $tasks.get()
    .filter(t => t.status === 'downloading' || t.status === 'connecting' || t.status === 'resuming')
    .map(t => t.id)

  if (ids.length === 0) {
    addToast(tr('toast.noTasksToPause'), 'info')
    return
  }

  await Promise.allSettled(ids.map(id => api.pauseTask(id)))
  await refreshTaskList()
}

export async function resumeAll(): Promise<void> {
  const ids = $tasks.get()
    .filter(t => t.status === 'paused')
    .map(t => t.id)

  if (ids.length === 0) {
    addToast(tr('toast.noTasksToResume'), 'info')
    return
  }

  await Promise.allSettled(ids.map(id => api.resumeTask(id)))
  await refreshTaskList()
}

/**
 * 取消所有运行中/暂停中的任务
 *
 * cancelAll 走单次应用内确认(中性 tone),避免误触批量取消。
 */
export async function cancelAll(): Promise<void> {
  const ids = $tasks.get()
    .filter(t => t.status === 'downloading' || t.status === 'connecting' || t.status === 'resuming' || t.status === 'paused')
    .map(t => t.id)

  if (ids.length === 0) {
    addToast(tr('toast.noTasksToCancel'), 'info')
    return
  }

  const result = await requestConfirm({
    title: tr('confirm.cancelBatch.title'),
    message: tr('confirm.cancelBatch.message', { count: ids.length }),
    confirmLabel: tr('confirm.cancelBatch.confirmLabel'),
  })
  if (!result.ok) return

  await Promise.allSettled(ids.map(id => api.cancelTask(id)))
  await refreshTaskList()
}
