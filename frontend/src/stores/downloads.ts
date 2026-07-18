import { errorMessage } from "../utils/appError";
import { createSignal, batch } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import type {
  TaskInfo,
  DownloadStatus,
  ProgressPayload,
  DownloadFilter,
} from "../types";
import { api } from "../api/invoke";
import { addToast } from "./toast";
import { addHistoryRecord } from "./history";
import { pushTaskSpeed } from "./taskSpeedHistory";
import {
  mergeFragmentDelta,
  getTaskFragmentData,
  loadTaskFragments,
  clearTaskFragmentDownloading,
} from "./taskFragments";
import { createRootMemo } from "../utils/reactive";
import { tr } from "../i18n";

// ── 高频进度数据(hot 层,250ms 级更新) ─────────────────────────
//
// 将进度/速度等高频变化字段拆分到独立 signal,避免每次 progress tick
// 触发 tasks store 的 reconcile,从而减少低频字段(文件名/URL/路径)
// 依赖组件的无谓重渲染。hot 层以 task id 为 key,只包含每帧真正变化的数值。

export interface HotProgress {
  downloaded: number;
  speed: number;
  progress: number;
  fragmentsDone: number;
}

const [hotProgress, setHotProgress] = createSignal<Map<string, HotProgress>>(
  new Map(),
);

const VALID_STATUSES = new Set<string>([
  "pending",
  "connecting",
  "downloading",
  "paused",
  "resuming",
  "verifying",
  "completed",
  "failed",
  "cancelled",
]);

const DOWNLOADING_STATUSES: DownloadStatus[] = [
  "connecting",
  "downloading",
  "resuming",
  "verifying",
];
const INCOMPLETE_STATUSES: DownloadStatus[] = [
  "pending",
  "connecting",
  "downloading",
  "paused",
  "resuming",
  "verifying",
];
const COMPLETED_STATUSES: DownloadStatus[] = ["completed"];

// 预构建 Set，将 .includes() 从 O(k) 降至 O(1)
const DOWNLOADING_SET = new Set<DownloadStatus>(DOWNLOADING_STATUSES);
const INCOMPLETE_SET = new Set<DownloadStatus>(INCOMPLETE_STATUSES);
const COMPLETED_SET = new Set<DownloadStatus>(COMPLETED_STATUSES);

const [tasks, setTasksRaw] = createStore<TaskInfo[]>([]);
const [selectedId, setSelectedId] = createSignal<string | null>(null);
const [currentFilter, setCurrentFilter] = createSignal<DownloadFilter>("all");

// 任务 ID → 数组索引映射，updateProgress 从 O(m*n) 降至 O(m)
let taskIndexMap = new Map<string, number>();

function rebuildIndexMap() {
  taskIndexMap = new Map<string, number>();
  for (let i = 0; i < tasks.length; i++) {
    taskIndexMap.set(tasks[i]!.id, i);
  }
}

export function setTasks(newTasks: TaskInfo[]) {
  batch(() => {
    setTasksRaw(reconcile(newTasks, { key: "id" }));
    rebuildIndexMap();
    // 同步初始化 hot 层:从全量任务列表提取高频字段
    const hotMap = new Map<string, HotProgress>();
    for (const t of newTasks) {
      hotMap.set(t.id, {
        downloaded: t.downloaded,
        speed: t.speed,
        progress: t.progress,
        fragmentsDone: t.fragmentsDone,
      });
    }
    setHotProgress(hotMap);
  });
}

export { setSelectedId, setCurrentFilter };

export const $hotProgress = {
  get: hotProgress,
};

/** 读取单任务 hot 进度;缺失时返回 undefined(调用方回退 cold task) */
export function getHotProgress(taskId: string): HotProgress | undefined {
  return hotProgress().get(taskId);
}

export const $tasks = {
  get: () => tasks,
  set: setTasks,
};

export const $selectedId = {
  get: selectedId,
  set: setSelectedId,
};

export const $currentFilter = {
  get: currentFilter,
  set: setCurrentFilter,
};

const filteredTasks = createRootMemo(() => {
  const filter = currentFilter();
  switch (filter) {
    case "downloading":
      return tasks.filter((t) => DOWNLOADING_SET.has(t.status));
    case "completed":
      return tasks.filter((t) => COMPLETED_SET.has(t.status));
    case "incomplete":
      return tasks.filter((t) => INCOMPLETE_SET.has(t.status));
    default:
      return tasks;
  }
});

export const $filteredTasks = {
  get: filteredTasks,
};

// 单次遍历统计四个计数器，替代原来 3 次独立 filter
const filterCounts = createRootMemo(() => {
  let downloading = 0;
  let completed = 0;
  let incomplete = 0;
  for (let i = 0; i < tasks.length; i++) {
    const s = tasks[i]!.status;
    if (DOWNLOADING_SET.has(s)) downloading++;
    if (COMPLETED_SET.has(s)) completed++;
    if (INCOMPLETE_SET.has(s)) incomplete++;
  }
  return { all: tasks.length, downloading, completed, incomplete };
});

export const $filterCounts = {
  get: filterCounts,
};

const selectedTask = createRootMemo(() => {
  const id = selectedId();
  if (!id) return null;
  return tasks.find((t) => t.id === id) ?? null;
});

export const $selectedTask = {
  get: selectedTask,
};

// totalSpeed 和 activeCount 从 hot 层读取,避免高频 progress tick
// 触发 tasks store 的 reconcile 导致低频字段依赖组件无谓重渲染
const speedStats = createRootMemo(() => {
  let speed = 0;
  let count = 0;
  const hot = hotProgress();
  for (let i = 0; i < tasks.length; i++) {
    if (DOWNLOADING_SET.has(tasks[i]!.status)) {
      const hp = hot.get(tasks[i]!.id);
      speed += hp?.speed ?? (tasks[i]!.speed || 0);
      count++;
    }
  }
  return { speed, count };
});

const totalSpeed = createRootMemo(() => speedStats().speed);
const activeCount = createRootMemo(() => speedStats().count);

export const $totalSpeed = {
  get: totalSpeed,
};

export const $activeCount = {
  get: activeCount,
};

export function updateProgress(payload: Record<string, ProgressPayload>) {
  const TERMINAL_STATUSES = new Set<DownloadStatus>([
    "completed",
    "failed",
    "cancelled",
  ]);

  batch(() => {
    // hot 层增量更新:收集所有变化的 high-frequency 字段
    const hotUpdates = new Map<string, HotProgress>();

    for (const [id, p] of Object.entries(payload)) {
      const idx = taskIndexMap.get(id); // O(1) 查找
      if (idx === undefined) continue;

      const task = tasks[idx]!;
      const oldStatus = task.status;
      const newStatus = VALID_STATUSES.has(p.status)
        ? (p.status as DownloadStatus)
        : oldStatus;

      const newDownloaded = p.downloaded ?? task.downloaded;
      const newSpeed = p.speed ?? task.speed;
      const newProgress = p.progress ?? task.progress;
      const newFragmentsDone = p.fragmentsDone ?? task.fragmentsDone;
      const newFragmentsTotal = p.fragmentsTotal ?? task.fragmentsTotal;
      const newConcurrency = p.activeConcurrency ?? 0;
      // 探测完成后后端通过进度事件同步 file_size,避免详情页显示 0B
      // (后端 #[serde(skip_serializing_if = "Option::is_none")] 省略空值,
      //  仅在探测完成有值时到达前端)
      const newSize = p.fileSize ?? task.fileSize;
      // 审计 FT-04:errorReason 显式 null 必须可清空;undefined 才表示字段缺失
      const newErrorReason =
        p.errorReason !== undefined
          ? (p.errorReason ?? undefined)
          : task.errorReason;

      // hot 层:高频字段变化时更新 hotProgress signal
      const hotChanged =
        newDownloaded !== task.downloaded ||
        newSpeed !== task.speed ||
        newProgress !== task.progress ||
        newFragmentsDone !== task.fragmentsDone;

      if (hotChanged) {
        hotUpdates.set(id, {
          downloaded: newDownloaded,
          speed: newSpeed,
          progress: newProgress,
          fragmentsDone: newFragmentsDone,
        });
      }

      // 记录下载中任务的单任务速度历史,供详情页速度趋势图使用
      if (newStatus === "downloading") {
        pushTaskSpeed(id, newSpeed);
      }

      // 审计 FT-04:cold 字段(fragmentsTotal/concurrency/errorReason)不能被
      // hot/status/size 三类条件代理,否则详情线程列与错误文案会卡在旧值
      const oldConcurrency = task.activeConcurrency ?? 0;
      const coldChanged =
        newFragmentsTotal !== task.fragmentsTotal ||
        newConcurrency !== oldConcurrency ||
        newErrorReason !== task.errorReason;
      const sizeChanged = newSize !== task.fileSize;
      const hasChanged =
        hotChanged || newStatus !== oldStatus || coldChanged || sizeChanged;

      // 只有至少一个字段真正变化时才更新 store，避免无意义 reconcile
      if (hasChanged) {
        setTasksRaw(idx, {
          downloaded: newDownloaded,
          speed: newSpeed,
          status: newStatus,
          progress: newProgress,
          fragmentsDone: newFragmentsDone,
          fragmentsTotal: newFragmentsTotal,
          fileSize: newSize,
          activeConcurrency: newConcurrency,
          errorReason: newErrorReason,
        });
      }

      // 合并分片 delta 到 fragment store
      // fragmentBytes 每 tick 推送(有活跃分片时),delta 为空也需触发合并
      const hasCompleted = p.completedDelta && p.completedDelta.length > 0;
      const hasStarted = p.startedDelta && p.startedDelta.length > 0;
      const hasFragmentBytes = p.fragmentBytes && p.fragmentBytes.length > 0;
      if (hasCompleted || hasStarted || hasFragmentBytes) {
        mergeFragmentDelta(
          id,
          p.completedDelta ?? [],
          p.startedDelta ?? [],
          p.fragmentBytes,
        );
      }

      // fragmentsTotal 从 0 变非 0:PlanComplete 到达,DetailPanel 若已打开需重拉。
      // 兼容 DetailPanel 在探测阶段提前打开的情况:此时 store 中可能尚无数据,
      // 或仅有无效空数据(已通过在 total=0 时不写入 store 避免),直接触发首拉。
      if (task.fragmentsTotal === 0 && newFragmentsTotal > 0) {
        const fragData = getTaskFragmentData(id);
        if (!fragData || fragData.total === 0) {
          loadTaskFragments(id);
        }
      }

      // 状态转 terminal：只在 status 真正变化到 terminal 时触发
      if (
        oldStatus !== newStatus &&
        !TERMINAL_STATUSES.has(oldStatus) &&
        TERMINAL_STATUSES.has(newStatus)
      ) {
        const updatedTask = tasks[idx]!;
        const duration = updatedTask.createdAt
          ? Date.now() - new Date(updatedTask.createdAt).getTime()
          : 0;
        const avgSpeed =
          duration > 0 ? (updatedTask.downloaded || 0) / (duration / 1000) : 0;

        addHistoryRecord({
          url: updatedTask.url,
          fileName: updatedTask.fileName,
          fileSize: updatedTask.fileSize || 0,
          status: newStatus as "completed" | "failed" | "cancelled",
          duration: Math.floor(duration / 1000), // 秒
          avgSpeed,
          savePath: updatedTask.savePath || "",
        });

        // 终态清理:清空 downloadingSet,避免残留 downloading 色格子
        // (后端 cleanup_runtime 已移除 fragment_state_store,前端需显式清空)
        clearTaskFragmentDownloading(id);
      }
    }

    // 批量更新 hot 层 signal
    if (hotUpdates.size > 0) {
      setHotProgress((prev) => {
        const next = new Map(prev);
        for (const [id, hp] of hotUpdates) {
          next.set(id, hp);
        }
        return next;
      });
    }
  });
}

export async function refreshTaskList() {
  try {
    const tasks = await api.getTaskList();
    setTasks(tasks);
  } catch (e) {
    addToast(tr("toast.refreshTasksFailed", { error: errorMessage(e) }), "error");
  }
}

/**
 * 手动重排任务顺序。
 *
 * 先乐观更新本地 store 以立即反馈,再异步调用后端持久化,
 * 失败时通过刷新回退到服务端状态。
 */
export async function reorderTasks(orderedIds: string[]) {
  const prev = tasks.slice();
  const idSet = new Set(orderedIds);
  const tail = tasks.filter((t) => !idSet.has(t.id));
  const sorted = [
    ...orderedIds.map((id) => tasks.find((t) => t.id === id)!),
    ...tail,
  ];
  setTasks(sorted);
  try {
    await api.reorderTasks(orderedIds);
  } catch (e) {
    setTasks(prev);
    addToast(tr("toast.reorderTasksFailed", { error: errorMessage(e) }), "error");
  }
}

/**
 * 将单个任务移动到指定任务之前。
 *
 * beforeId 为空时移动到列表末尾。
 */
export async function moveTask(taskId: string, beforeId?: string) {
  try {
    await api.moveTask(taskId, beforeId);
    await refreshTaskList();
  } catch (e) {
    addToast(tr("toast.reorderTasksFailed", { error: errorMessage(e) }), "error");
  }
}
