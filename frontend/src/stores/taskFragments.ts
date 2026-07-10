import { createSignal } from "solid-js";
import { api } from "../api/invoke";

// 单任务分片数据:真实 doneSet + downloadingSet + 并发度
interface TaskFragmentData {
  total: number;
  concurrency: number;
  doneSet: Set<number>;
  downloadingSet: Set<number>;
}

const [fragmentMap, setFragmentMap] = createSignal<Map<string, TaskFragmentData>>(
  new Map(),
);

// 竞态防护 token:DetailPanel task 切换时,旧的 loadTaskFragments 返回被丢弃
let currentLoadToken = 0;

/** DetailPanel 打开/task 切换时调用:首拉元数据 + 初始 doneSet/downloadingSet
 * 后端 total=0 表示 PlanComplete 尚未到达(探测中),此时不写入 store,
 * 保持 undefined 以便后续 fragmentsTotal 变非 0 时再次重拉。 */
export async function loadTaskFragments(taskId: string) {
  const token = ++currentLoadToken;
  const view = await api.getTaskFragments(taskId);
  if (token !== currentLoadToken) return; // 已被后续切换覆盖,丢弃
  if (view.total === 0) return; // 分片尚未规划完成,不缓存空数据
  const doneSet = new Set<number>(view.doneIndices);
  const downloadingSet = new Set<number>(view.downloadingIndices ?? []);
  setFragmentMap((prev) => {
    const next = new Map(prev);
    next.set(taskId, { total: view.total, concurrency: 0, doneSet, downloadingSet });
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

/** updateProgress 调用:合并 completed/started delta + 更新 concurrency
 *
 * 优先级合并(done > downloading):同一 250ms 窗口内若分片同时出现在
 * startedDelta 和 completedDelta,先处理 completed(进 doneSet),
 * 再加入 started 时检查 !doneSet.has(idx) 跳过--保证不会同时存在于两个集合。 */
export function mergeFragmentDelta(
  taskId: string,
  completedDelta: number[],
  startedDelta: number[],
  concurrency: number,
) {
  setFragmentMap((prev) => {
    const data = prev.get(taskId);
    if (!data) return prev; // DetailPanel 未打开,忽略(后续首拉拿完整快照)
    const next = new Map(prev);
    // 先处理 completed:加入 doneSet,从 downloadingSet 移除
    const newDone = new Set(data.doneSet);
    const newDownloading = new Set(data.downloadingSet);
    for (const idx of completedDelta) {
      newDone.add(idx);
      newDownloading.delete(idx);
    }
    // 再处理 started:跳过已完成的(防御同窗口竞态)
    for (const idx of startedDelta) {
      if (!newDone.has(idx)) newDownloading.add(idx);
    }
    next.set(taskId, {
      ...data,
      doneSet: newDone,
      downloadingSet: newDownloading,
      concurrency: concurrency || data.concurrency,
    });
    return next;
  });
}

/** ChunkMatrix 读取:获取任务分片数据 */
export function getTaskFragmentData(taskId: string) {
  return fragmentMap().get(taskId);
}
