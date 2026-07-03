import { createSignal } from "solid-js";
import { api } from "../api/invoke";

// 单任务分片数据:真实 doneSet + 并发度
interface TaskFragmentData {
  total: number;
  concurrency: number;
  doneSet: Set<number>;
}

const [fragmentMap, setFragmentMap] = createSignal<Map<string, TaskFragmentData>>(
  new Map(),
);

// 竞态防护 token:DetailPanel task 切换时,旧的 loadTaskFragments 返回被丢弃
let currentLoadToken = 0;

/** DetailPanel 打开/task 切换时调用:首拉元数据 + 初始 doneSet */
export async function loadTaskFragments(taskId: string) {
  const token = ++currentLoadToken;
  const view = await api.getTaskFragments(taskId);
  if (token !== currentLoadToken) return; // 已被后续切换覆盖,丢弃
  const doneSet = new Set<number>(view.doneIndices);
  setFragmentMap((prev) => {
    const next = new Map(prev);
    next.set(taskId, { total: view.total, concurrency: 0, doneSet });
    return next;
  });
}

/** DetailPanel 关闭时调用:清理 */
export function clearTaskFragments(taskId: string) {
  setFragmentMap((prev) => {
    const next = new Map(prev);
    next.delete(taskId);
    return next;
  });
}

/** updateProgress 调用:合并 delta + 更新 concurrency */
export function mergeFragmentDelta(
  taskId: string,
  delta: number[],
  concurrency: number,
) {
  setFragmentMap((prev) => {
    const data = prev.get(taskId);
    if (!data) return prev; // DetailPanel 未打开,忽略(后续首拉拿完整 doneSet)
    const next = new Map(prev);
    const newSet = new Set(data.doneSet);
    for (const idx of delta) newSet.add(idx);
    next.set(taskId, {
      ...data,
      doneSet: newSet,
      concurrency: concurrency || data.concurrency,
    });
    return next;
  });
}

/** ChunkMatrix 读取:获取任务分片数据 */
export function getTaskFragmentData(taskId: string) {
  return fragmentMap().get(taskId);
}
