import type { DownloadStatus } from "../types";

/**
 * 任务列表按状态分组定义。
 *
 * 固定分组顺序与状态映射:
 * - active: 正在活跃流转的状态
 * - pending: 等待调度
 * - paused: 已暂停
 * - completed: 已完成
 * - failed: 失败
 * - cancelled: 已取消
 */

export type GroupKey =
  | "active"
  | "pending"
  | "paused"
  | "completed"
  | "failed"
  | "cancelled";

export const GROUP_ORDER: GroupKey[] = [
  "active",
  "pending",
  "paused",
  "completed",
  "failed",
  "cancelled",
];

export const STATUS_TO_GROUP: Record<DownloadStatus, GroupKey> = {
  downloading: "active",
  connecting: "active",
  resuming: "active",
  verifying: "active",
  pending: "pending",
  paused: "paused",
  completed: "completed",
  failed: "failed",
  cancelled: "cancelled",
};

export const GROUP_STATUSES: Record<GroupKey, DownloadStatus[]> = {
  active: ["downloading", "connecting", "resuming", "verifying"],
  pending: ["pending"],
  paused: ["paused"],
  completed: ["completed"],
  failed: ["failed"],
  cancelled: ["cancelled"],
};

export function getTaskGroup(status: DownloadStatus): GroupKey {
  return STATUS_TO_GROUP[status];
}
