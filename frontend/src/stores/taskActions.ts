import type { TaskInfo } from "../types";
import { api } from "../api/invoke";

/**
 * 终态任务集合:与后端 create_task 去重排除口径一致
 * (TaskService::create_task 仅排除 cancelled/completed/failed)。
 */
const TERMINAL_STATUSES = new Set(["completed", "failed", "cancelled"]);

/**
 * 重新下载:为任务创建同 URL 的新任务。
 *
 * 后端 create_task 按 url_identity_key 去重:同 URL 存在非终态任务时
 * 拒绝创建("相同 URL 的下载任务已存在"),导致对活跃任务点「重新下载」
 * 必然失败。因此对活跃任务先取消——后端 cancel_task 会等待旧 task_fn
 * 静默退出(H-04),避免与新建任务写同一目标文件的竞态——再创建新任务
 * 从头下载。旧任务以 cancelled 记录保留,可手动删除或撤销。
 */
export async function redownloadTask(task: TaskInfo): Promise<void> {
  if (!TERMINAL_STATUSES.has(task.status)) {
    // 取消失败不阻断创建(如任务恰好转入终态):createTask 的去重
    // 结果才是最终成败判据,其错误会向调用方抛出
    await api.cancelTask(task.id).catch(() => {});
  }
  await api.createTask(task.url);
}
