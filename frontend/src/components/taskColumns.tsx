/**
 * 任务列表列定义（Iteration 07, DI-3 + 列自定义扩展）。
 *
 * 单一数据来源：TaskList 表头与 TaskItem 数据行共用本配置，
 * 消除原「表头 120px/100px/80px 与行内手写同样数值」的两处对齐。
 * 支持列排序(sortable)，供 taskSort store 驱动。
 */

import type { JSX } from "solid-js";
import type { MessageKey } from "../i18n";
import type { TaskInfo } from "../types";
import { formatDate, formatSize, formatSpeed } from "../utils/format";
import { tr } from "../i18n";
import StatusBadge from "./StatusBadge";

export type ColumnKey =
  | "name"
  | "progress"
  | "speed"
  | "status"
  | "size"
  | "remaining"
  | "downloaded"
  | "fragments"
  | "threads"
  | "createdAt";

export type SortKey = ColumnKey;
export type SortDir = "asc" | "desc";

export interface ColumnDef {
  key: ColumnKey;
  /** i18n key（渲染时翻译） */
  labelKey: MessageKey;
  /** 默认宽度。'flex-1' 表示弹性填充（文件名列） */
  defaultWidth: number | "flex-1";
  /** 最小宽度（px），拖拽时 clamp 用 */
  minWidth: number;
  align: "left" | "right";
  /** 是否支持点击排序 */
  sortable: boolean;
}

/** 默认可见列 */
export const DEFAULT_VISIBLE_KEYS: ColumnKey[] = [
  "name",
  "progress",
  "speed",
  "status",
];

/** 列定义（顺序 = 渲染顺序） */
export const ALL_COLUMNS: ColumnDef[] = [
  {
    key: "name",
    labelKey: "taskList.column.name",
    defaultWidth: "flex-1",
    minWidth: 160,
    align: "left",
    sortable: false,
  },
  {
    key: "progress",
    labelKey: "taskList.column.progress",
    defaultWidth: 120,
    minWidth: 60,
    align: "right",
    sortable: true,
  },
  {
    key: "speed",
    labelKey: "taskList.column.speed",
    defaultWidth: 100,
    minWidth: 60,
    align: "right",
    sortable: true,
  },
  {
    key: "status",
    labelKey: "taskList.column.status",
    defaultWidth: 80,
    minWidth: 48,
    align: "right",
    sortable: true,
  },
  {
    key: "size",
    labelKey: "taskList.column.size",
    defaultWidth: 100,
    minWidth: 60,
    align: "right",
    sortable: true,
  },
  {
    key: "remaining",
    labelKey: "taskList.column.remaining",
    defaultWidth: 100,
    minWidth: 60,
    align: "right",
    sortable: true,
  },
  {
    key: "downloaded",
    labelKey: "taskList.column.downloaded",
    defaultWidth: 100,
    minWidth: 60,
    align: "right",
    sortable: true,
  },
  {
    key: "fragments",
    labelKey: "taskList.column.fragments",
    defaultWidth: 90,
    minWidth: 48,
    align: "right",
    sortable: true,
  },
  {
    key: "threads",
    labelKey: "taskList.column.concurrency",
    defaultWidth: 80,
    minWidth: 48,
    align: "right",
    sortable: true,
  },
  {
    key: "createdAt",
    labelKey: "taskList.column.createdAt",
    defaultWidth: 140,
    minWidth: 80,
    align: "right",
    sortable: true,
  },
];

/** 兼容旧导出：完整列定义 */
export const COLUMNS = ALL_COLUMNS;

/** 可排序列 */
export const SORTABLE_KEYS: SortKey[] = ALL_COLUMNS.filter(
  (c) => c.sortable,
).map((c) => c.key);

/**
 * 列宽常量（兼容旧代码；新代码优先使用 $taskColumns.width）。
 * 文件名列用 flex-1，此处不列。
 */
export const COLUMN_WIDTH = {
  progress: "120px",
  speed: "100px",
  status: "80px",
} as const;

/** 单元格渲染函数类型 */
export type CellRenderer = (
  task: TaskInfo,
  opts: { isCompact: boolean },
) => JSX.Element | string;

/** 各列单元格渲染器。name 列由 TaskItem 内部特殊处理，此处返回空占位。 */
export const COLUMN_CELL_RENDERERS: Record<ColumnKey, CellRenderer> = {
  name: () => "",
  progress: (task) => `${(task.progress * 100).toFixed(1)}%`,
  speed: (task) => formatSpeed(task.speed),
  status: (task) => <StatusBadge status={task.status} showIcon size="sm" />,
  size: (task) =>
    task.fileSize ? formatSize(task.fileSize) : tr("taskList.unknownSize"),
  remaining: (task) =>
    task.fileSize
      ? formatSize(task.fileSize - task.downloaded)
      : tr("taskList.unknownSize"),
  downloaded: (task) => formatSize(task.downloaded),
  fragments: (task) => `${task.fragmentsDone}/${task.fragmentsTotal}`,
  threads: (task) => `${task.activeConcurrency ?? "-"}`,
  createdAt: (task) => formatDate(task.createdAt),
};
