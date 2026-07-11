import { api } from "../api/invoke";
import { errorMessage } from "../utils/appError";
import { $tasks, refreshTaskList } from "./downloads";
import { $selectedIds, deselectAll } from "./selection";
import { addToast } from "./toast";
import { requestConfirm } from "./confirm";
import { clearTaskHistory } from "./taskSpeedHistory";
import { tr } from "../i18n";

export async function pauseSelected(): Promise<void> {
  const ids = Array.from($selectedIds.get());
  if (ids.length === 0) return;

  const results = await Promise.allSettled(ids.map((id) => api.pauseTask(id)));
  const failures = results.filter(
    (r): r is PromiseRejectedResult => r.status === "rejected",
  );
  const successes = ids.length - failures.length;

  if (successes > 0) {
    addToast(tr("toast.pauseBatchSuccess", { count: successes }), "success");
  }
  if (failures.length > 0) {
    addToast(
      tr("toast.pauseBatchPartialFailed", {
        count: failures.length,
        error: failures[0]?.reason ?? "",
      }),
      "error",
    );
  }
  deselectAll();
  await refreshTaskList();
}

export async function resumeSelected(): Promise<void> {
  const ids = Array.from($selectedIds.get());
  if (ids.length === 0) return;

  const results = await Promise.allSettled(ids.map((id) => api.resumeTask(id)));
  const failures = results.filter(
    (r): r is PromiseRejectedResult => r.status === "rejected",
  );
  const successes = ids.length - failures.length;

  if (successes > 0) {
    addToast(tr("toast.resumeBatchSuccess", { count: successes }), "success");
  }
  if (failures.length > 0) {
    addToast(
      tr("toast.resumeBatchPartialFailed", {
        count: failures.length,
        error: failures[0]?.reason ?? "",
      }),
      "error",
    );
  }
  deselectAll();
  await refreshTaskList();
}

/**
 * 批量取消选中任务
 *
 * cancel = 立即停止下载但保留任务记录(区别于 delete)。cancel_task 是 mutate
 * 级(非 destructive),后端无需 confirmation token;但批量操作为防误触,前端
 * 走一次应用内 ConfirmDialog(中性 tone,提示"停止但保留记录")。
 */
export async function cancelSelected(): Promise<void> {
  const ids = Array.from($selectedIds.get());
  if (ids.length === 0) return;

  const result = await requestConfirm({
    title: tr("confirm.cancelBatch.title"),
    message: tr("confirm.cancelBatch.message", { count: ids.length }),
    confirmLabel: tr("confirm.cancelBatch.confirmLabel"),
  });
  if (!result.ok) return;

  const results = await Promise.allSettled(ids.map((id) => api.cancelTask(id)));
  const failures = results.filter(
    (r): r is PromiseRejectedResult => r.status === "rejected",
  );
  const successes = ids.length - failures.length;

  if (successes > 0) {
    addToast(tr("toast.cancelBatchSuccess", { count: successes }), "success");
  }
  if (failures.length > 0) {
    addToast(
      tr("toast.cancelBatchPartialFailed", {
        count: failures.length,
        error: failures[0]?.reason ?? "",
      }),
      "error",
    );
  }
  deselectAll();
  await refreshTaskList();
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
  const ids = Array.from($selectedIds.get());
  if (ids.length === 0) return;

  const result = await requestConfirm({
    title: tr("confirm.deleteBatch.title"),
    message: tr("confirm.deleteBatch.message", { count: ids.length }),
    confirmLabel: tr("confirm.delete.confirmLabel"),
    tone: "danger",
    showDeleteLocalFileOption: true,
    deleteLocalFileDefault: false,
  });
  if (!result.ok) return;

  const results = await Promise.allSettled(
    ids.map((id) =>
      api.deleteTask(id, {
        skipConfirm: true,
        deleteLocalFile: result.deleteLocalFile,
      }),
    ),
  );
  const failures = results.filter(
    (r): r is PromiseRejectedResult => r.status === "rejected",
  );
  const successes = ids.length - failures.length;

  if (successes > 0) {
    addToast(tr("toast.deleteBatchSuccess", { count: successes }), "success");
  }
  if (failures.length > 0) {
    addToast(
      tr("toast.deleteBatchPartialFailed", {
        count: failures.length,
        error: failures[0]?.reason ?? "",
      }),
      "error",
    );
  }
  ids.forEach((id) => clearTaskHistory(id));
  deselectAll();
  await refreshTaskList();
}

export async function pauseAll(): Promise<void> {
  const ids = $tasks
    .get()
    .filter(
      (t) =>
        t.status === "downloading" ||
        t.status === "connecting" ||
        t.status === "resuming",
    )
    .map((t) => t.id);

  if (ids.length === 0) {
    addToast(tr("toast.noTasksToPause"), "info");
    return;
  }

  await Promise.allSettled(ids.map((id) => api.pauseTask(id)));
  await refreshTaskList();
}

export async function resumeAll(): Promise<void> {
  const ids = $tasks
    .get()
    .filter((t) => t.status === "paused")
    .map((t) => t.id);

  if (ids.length === 0) {
    addToast(tr("toast.noTasksToResume"), "info");
    return;
  }

  await Promise.allSettled(ids.map((id) => api.resumeTask(id)));
  await refreshTaskList();
}

/**
 * 取消所有运行中/暂停中的任务
 *
 * cancelAll 走单次应用内确认(中性 tone),避免误触批量取消。
 */
export async function cancelAll(): Promise<void> {
  const ids = $tasks
    .get()
    .filter(
      (t) =>
        t.status === "downloading" ||
        t.status === "connecting" ||
        t.status === "resuming" ||
        t.status === "paused",
    )
    .map((t) => t.id);

  if (ids.length === 0) {
    addToast(tr("toast.noTasksToCancel"), "info");
    return;
  }

  const result = await requestConfirm({
    title: tr("confirm.cancelBatch.title"),
    message: tr("confirm.cancelBatch.message", { count: ids.length }),
    confirmLabel: tr("confirm.cancelBatch.confirmLabel"),
  });
  if (!result.ok) return;

  await Promise.allSettled(ids.map((id) => api.cancelTask(id)));
  await refreshTaskList();
}

/**
 * 删除所有已完成任务记录
 *
 * 与 deleteSelected 一致:单次应用内确认(danger tone),每个 deleteTask
 * 传 skipConfirm:true 跳过 invoke 内置 window.confirm。
 */
export async function clearCompleted(): Promise<void> {
  const ids = $tasks
    .get()
    .filter((t) => t.status === "completed")
    .map((t) => t.id);

  if (ids.length === 0) {
    addToast(tr("toast.noTasksToClear"), "info");
    return;
  }

  const result = await requestConfirm({
    title: tr("confirm.clearCompleted.title"),
    message: tr("confirm.clearCompleted.message", { count: ids.length }),
    confirmLabel: tr("confirm.clearCompleted.confirmLabel"),
    tone: "danger",
    showDeleteLocalFileOption: true,
    deleteLocalFileDefault: false,
  });
  if (!result.ok) return;

  const results = await Promise.allSettled(
    ids.map((id) =>
      api.deleteTask(id, {
        skipConfirm: true,
        deleteLocalFile: result.deleteLocalFile,
      }),
    ),
  );
  const failures = results.filter(
    (r): r is PromiseRejectedResult => r.status === "rejected",
  );
  const successes = ids.length - failures.length;

  if (successes > 0) {
    addToast(tr("toast.clearCompletedSuccess", { count: successes }), "success");
  }
  if (failures.length > 0) {
    addToast(
      tr("toast.clearCompletedPartialFailed", {
        count: failures.length,
        error: failures[0]?.reason ?? "",
      }),
      "error",
    );
  }
  ids.forEach((id) => clearTaskHistory(id));
  await refreshTaskList();
}

/**
 * 批量打开选中任务的保存目录
 *
 * 对 completed / failed / cancelled 等终态任务,savePath 通常已确定;
 * 对尚未开始或探测中的任务可能无路径,此时跳过并提示。
 */
export async function openSelectedFolders(): Promise<void> {
  const ids = Array.from($selectedIds.get());
  if (ids.length === 0) return;

  const tasks = $tasks.get();
  let opened = 0;
  let missing = 0;
  const failures: string[] = [];

  await Promise.allSettled(
    ids.map(async (id) => {
      const task = tasks.find((t) => t.id === id);
      if (!task?.savePath) {
        missing++;
        return;
      }
      try {
        await api.openFolder(task.savePath);
        opened++;
      } catch (e) {
        failures.push(errorMessage(e));
      }
    }),
  );

  if (opened > 0) {
    addToast(tr("toast.openFolderBatchSuccess", { count: opened }), "success");
  }
  if (missing > 0) {
    addToast(tr("toast.openFolderBatchNoPath", { count: missing }), "info");
  }
  if (failures.length > 0) {
    addToast(
      tr("toast.openFolderBatchFailed", {
        count: failures.length,
        error: failures[0] ?? "",
      }),
      "error",
    );
  }
}

/**
 * 批量复制选中任务的下载链接到剪贴板
 *
 * 多个 URL 以换行分隔;复制是非破坏性操作,保持选中状态以便用户继续操作。
 */
export async function copySelectedLinks(): Promise<void> {
  const ids = Array.from($selectedIds.get());
  if (ids.length === 0) return;

  const tasks = $tasks.get();
  const urls = ids
    .map((id) => tasks.find((t) => t.id === id)?.url)
    .filter((url): url is string => Boolean(url));

  if (urls.length === 0) {
    addToast(tr("toast.copyLinkBatchNoUrl"), "info");
    return;
  }

  try {
    await navigator.clipboard.writeText(urls.join("\n"));
    addToast(
      tr("toast.copyLinkBatchSuccess", { count: urls.length }),
      "success",
    );
  } catch (e) {
    addToast(
      tr("toast.copyLinkBatchFailed", { error: errorMessage(e) }),
      "error",
    );
  }
}

/**
 * 批量重新下载选中任务
 *
 * 为每个选中任务的 url 创建新任务;创建完成后清空选择并刷新列表。
 */
export async function redownloadSelected(): Promise<void> {
  const ids = Array.from($selectedIds.get());
  if (ids.length === 0) return;

  const tasks = $tasks.get();
  const urls = ids
    .map((id) => tasks.find((t) => t.id === id)?.url)
    .filter((url): url is string => Boolean(url));

  if (urls.length === 0) {
    addToast(tr("toast.redownloadBatchNoUrl"), "info");
    return;
  }

  const results = await Promise.allSettled(
    urls.map((url) => api.createTask(url)),
  );
  const failures = results.filter(
    (r): r is PromiseRejectedResult => r.status === "rejected",
  );
  const successes = results.length - failures.length;

  if (successes > 0) {
    addToast(
      tr("toast.redownloadBatchSuccess", { count: successes }),
      "success",
    );
  }
  if (failures.length > 0) {
    addToast(
      tr("toast.redownloadBatchPartialFailed", {
        count: failures.length,
        error: failures[0]?.reason ?? "",
      }),
      "error",
    );
  }
  deselectAll();
  await refreshTaskList();
}
