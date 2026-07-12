import { createSignal } from "solid-js";
import { api } from "../api/invoke";

// 单任务分片数据:真实 doneSet + downloadingSet
export interface TaskFragmentData {
  total: number;
  doneSet: Set<number>;
  downloadingSet: Set<number>;
  /** 终态标记:true 时拒绝合并 downloading delta,防止延迟事件导致幽灵格子 */
  finalized: boolean;
}

const [fragmentMap, setFragmentMap] = createSignal<Map<string, TaskFragmentData>>(
  new Map(),
);

// 竞态防护 token:按 taskId 隔离,避免不同任务的并发 loadTaskFragments 互相干扰
const loadTokens = new Map<string, number>();

/** DetailPanel 打开/task 切换时调用:首拉元数据 + 初始 doneSet/downloadingSet
 * 后端 total=0 表示 PlanComplete 尚未到达(探测中),此时不写入 store,
 * 保持 undefined 以便后续 fragmentsTotal 变非 0 时再次重拉。
 *
 * 快照合并:await 期间收到的 delta 不会被覆盖。doneSet 取快照与本地 delta 的并集
 * (快照是权威基线,delta 是增量补充);downloadingSet 以快照为准
 * (后端 authoritative,本地 delta 可能已过时)。 */
export async function loadTaskFragments(taskId: string) {
  const token = (loadTokens.get(taskId) ?? 0) + 1;
  loadTokens.set(taskId, token);
  const view = await api.getTaskFragments(taskId);
  if (loadTokens.get(taskId) !== token) return; // 已被后续同 task 加载覆盖,丢弃
  if (view.total === 0) return; // 分片尚未规划完成,不缓存空数据
  const snapshotDone = new Set<number>(view.doneIndices);
  const snapshotDownloading = new Set<number>(view.downloadingIndices ?? []);
  setFragmentMap((prev) => {
    const data = prev.get(taskId);
    const next = new Map(prev);
    if (data) {
      // 合并:快照 doneSet 与本地 delta 已合并的 doneSet 取并集
      // (await 期间可能收到 completedDelta,这些不应被快照覆盖)
      const mergedDone = new Set(snapshotDone);
      for (const idx of data.doneSet) mergedDone.add(idx);
      // downloadingSet 以快照为准(后端 authoritative)
      next.set(taskId, {
        total: view.total,
        doneSet: mergedDone,
        downloadingSet: snapshotDownloading,
        finalized: data.finalized,
      });
    } else {
      next.set(taskId, {
        total: view.total,
        doneSet: snapshotDone,
        downloadingSet: snapshotDownloading,
        finalized: false,
      });
    }
    return next;
  });
}

/** DetailPanel 关闭时调用:清理 */
export function clearTaskFragments(taskId: string) {
  loadTokens.delete(taskId);
  setFragmentMap((prev) => {
    const next = new Map(prev);
    next.delete(taskId);
    return next;
  });
}

/** updateProgress 调用:合并 completed/started delta
 *
 * 优先级合并(done > downloading):同一 250ms 窗口内若分片同时出现在
 * startedDelta 和 completedDelta,先处理 completed(进 doneSet),
 * 再加入 started 时检查 !doneSet.has(idx) 跳过--保证不会同时存在于两个集合。
 *
 * 终态守卫:已 finalized 的任务拒绝 started delta(防延迟事件导致幽灵格子),
 * 仍允许 completedDelta 通过(补全可能缺失的 done 状态)。 */
export function mergeFragmentDelta(
  taskId: string,
  completedDelta: number[],
  startedDelta: number[],
) {
  setFragmentMap((prev) => {
    const data = prev.get(taskId);
    if (!data) return prev; // DetailPanel 未打开,忽略(后续首拉拿完整快照)
    // delta 均为空时短路:无状态变更,避免不必要的 Set 拷贝
    if (completedDelta.length === 0 && startedDelta.length === 0) return prev;
    // finalized 时只处理 completed,跳过所有 started
    const effectiveStarted = data.finalized ? [] : startedDelta;
    const next = new Map(prev);
    // 先处理 completed:加入 doneSet,从 downloadingSet 移除
    const newDone = new Set(data.doneSet);
    const newDownloading = new Set(data.downloadingSet);
    for (const idx of completedDelta) {
      newDone.add(idx);
      newDownloading.delete(idx);
    }
    // 再处理 started:跳过已完成的(防御同窗口竞态)
    for (const idx of effectiveStarted) {
      if (!newDone.has(idx)) newDownloading.add(idx);
    }
    next.set(taskId, {
      ...data,
      doneSet: newDone,
      downloadingSet: newDownloading,
    });
    return next;
  });
}

/** ChunkMatrix 读取:获取任务分片数据 */
export function getTaskFragmentData(taskId: string) {
  return fragmentMap().get(taskId);
}

/** 终态清理:清空 downloadingSet(任务完成/失败/取消后,后端 fragment_state_store
 * 已被 cleanup_runtime 移除,前端残留的 downloading 格子需显式清空)
 * 同时标记 finalized,拒绝后续延迟的 started delta 防止幽灵格子 */
export function clearTaskFragmentDownloading(taskId: string) {
  setFragmentMap((prev) => {
    const data = prev.get(taskId);
    if (!data || (data.downloadingSet.size === 0 && data.finalized)) return prev;
    const next = new Map(prev);
    next.set(taskId, { ...data, downloadingSet: new Set(), finalized: true });
    return next;
  });
}
