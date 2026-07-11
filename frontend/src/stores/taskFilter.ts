/**
 * 任务列表过滤/搜索/排序视图状态 store。
 *
 * 视图状态持久化到 localStorage key: tachyon.tasklist.view
 * 包括：搜索关键词、状态筛选、文件类型筛选、排序 key/direction。
 * 加载时校验非法值并回退到默认值。
 */

import { createRoot, createEffect, createSignal, type Accessor } from "solid-js";
import type { TaskInfo, SidebarFilter, FileTypeFilter } from "../types";
import type { SortKey, SortDir } from "../components/taskColumns";
import { $tasks } from "./downloads";
import { EXT_TYPE_MAP } from "../utils/format";
import { createRootMemo } from "../utils/reactive";

const STORAGE_KEY = "tachyon.tasklist.view";

export interface SortState {
  key: SortKey | null;
  dir: SortDir;
}

interface FilterState {
  searchQuery: string;
  sidebarFilter: SidebarFilter;
  fileTypeFilter: FileTypeFilter;
}

interface TaskCounts {
  all: number;
  downloading: number;
  completed: number;
  paused: number;
  failed: number;
}

interface ViewState {
  searchQuery: string;
  sidebarFilter: SidebarFilter;
  fileTypeFilter: FileTypeFilter;
  sort: SortState;
}

const DEFAULT_SEARCH_QUERY = "";
const DEFAULT_SIDEBAR_FILTER: SidebarFilter = "all";
const DEFAULT_FILE_TYPE_FILTER: FileTypeFilter = "all";
const DEFAULT_SORT_STATE: SortState = { key: null, dir: "desc" };

const VALID_SIDEBAR_FILTERS: SidebarFilter[] = [
  "all",
  "downloading",
  "completed",
  "paused",
  "failed",
];
const VALID_FILE_TYPE_FILTERS: FileTypeFilter[] = [
  "all",
  "video",
  "audio",
  "document",
  "image",
  "archive",
  "executable",
  "model",
  "other",
];
const VALID_SORT_KEYS: SortKey[] = [
  "name",
  "progress",
  "speed",
  "status",
  "size",
  "remaining",
  "downloaded",
  "fragments",
  "threads",
  "createdAt",
];
const VALID_SORT_DIRS: SortDir[] = ["asc", "desc"];

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isValidSidebarFilter(value: unknown): value is SidebarFilter {
  return (
    typeof value === "string" &&
    (VALID_SIDEBAR_FILTERS as string[]).includes(value)
  );
}

function isValidFileTypeFilter(value: unknown): value is FileTypeFilter {
  return (
    typeof value === "string" &&
    (VALID_FILE_TYPE_FILTERS as string[]).includes(value)
  );
}

function isValidSortKey(value: unknown): value is SortKey | null {
  return (
    value === null ||
    (typeof value === "string" && (VALID_SORT_KEYS as string[]).includes(value))
  );
}

function isValidSortDir(value: unknown): value is SortDir {
  return (
    typeof value === "string" && (VALID_SORT_DIRS as string[]).includes(value)
  );
}

function isValidSortState(value: unknown): value is SortState {
  return isObject(value) && isValidSortKey(value.key) && isValidSortDir(value.dir);
}

function getDefaultState(): ViewState {
  return {
    searchQuery: DEFAULT_SEARCH_QUERY,
    sidebarFilter: DEFAULT_SIDEBAR_FILTER,
    fileTypeFilter: DEFAULT_FILE_TYPE_FILTER,
    sort: DEFAULT_SORT_STATE,
  };
}

function load(): ViewState {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return getDefaultState();
    const parsed = JSON.parse(raw) as unknown;
    if (!isObject(parsed)) return getDefaultState();
    return {
      searchQuery:
        typeof parsed.searchQuery === "string"
          ? parsed.searchQuery
          : DEFAULT_SEARCH_QUERY,
      sidebarFilter: isValidSidebarFilter(parsed.sidebarFilter)
        ? parsed.sidebarFilter
        : DEFAULT_SIDEBAR_FILTER,
      fileTypeFilter: isValidFileTypeFilter(parsed.fileTypeFilter)
        ? parsed.fileTypeFilter
        : DEFAULT_FILE_TYPE_FILTER,
      sort: isValidSortState(parsed.sort) ? parsed.sort : DEFAULT_SORT_STATE,
    };
  } catch {
    return getDefaultState();
  }
}

function save(state: ViewState): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    /* ignore */
  }
}

const loaded = load();

const [searchQuery, setSearchQuery] = createSignal<string>(loaded.searchQuery);
const [sidebarFilter, setSidebarFilter] = createSignal<SidebarFilter>(
  loaded.sidebarFilter,
);
const [fileTypeFilter, setFileTypeFilter] = createSignal<FileTypeFilter>(
  loaded.fileTypeFilter,
);
const [sortState, setSortState] = createSignal<SortState>(loaded.sort);

const searchFilters = createRootMemo(() => {
  const query = searchQuery().trim();
  const filters: { type: string; value: string; raw: string }[] = [];
  let textQuery = query;

  const filterRegex = /(\w+):([^\s]+)/g;
  let match: RegExpExecArray | null;
  while ((match = filterRegex.exec(query)) !== null) {
    const type = match[1] ?? "";
    const value = match[2] ?? "";
    if (
      type &&
      value &&
      ["status", "type", "size", "speed", "name"].includes(type)
    ) {
      filters.push({ type, value, raw: match[0] });
      textQuery = textQuery.replace(match[0], "").trim();
    }
  }

  return { filters, textQuery };
});

function parseSize(val: string): number {
  const v = val.trim().toLowerCase();
  const num = parseFloat(v);
  if (Number.isNaN(num)) return 0;
  if (v.includes("gb")) return num * 1024 * 1024 * 1024;
  if (v.includes("mb")) return num * 1024 * 1024;
  if (v.includes("kb")) return num * 1024;
  return num;
}

function getFileTypeByName(fileName: string): string {
  const ext = fileName.split(".").pop()?.toLowerCase() ?? "";
  return EXT_TYPE_MAP[ext] ?? "other";
}

function computeFilteredTasks(
  tasks: TaskInfo[],
  sf: SidebarFilter,
  tf: FileTypeFilter,
  filters: { type: string; value: string; raw: string }[],
  textQuery: string,
): TaskInfo[] {
  let result = tasks;

  if (sf !== "all") {
    result = result.filter((t) => {
      if (sf === "downloading")
        return (
          t.status === "downloading" ||
          t.status === "connecting" ||
          t.status === "resuming" ||
          t.status === "verifying"
        );
      if (sf === "completed") return t.status === "completed";
      if (sf === "paused") return t.status === "paused";
      if (sf === "failed") return t.status === "failed";
      return true;
    });
  }

  if (tf !== "all") {
    result = result.filter((t) => getFileTypeByName(t.fileName) === tf);
  }

  for (const filter of filters) {
    result = result.filter((t) => {
      if (filter.type === "status") {
        return t.status.toLowerCase() === filter.value.toLowerCase();
      }
      if (filter.type === "type") {
        return getFileTypeByName(t.fileName) === filter.value.toLowerCase();
      }
      if (filter.type === "size") {
        if (!t.fileSize) return false;
        const val = filter.value.toLowerCase();
        if (val.startsWith(">")) {
          const num = parseSize(val.slice(1));
          return t.fileSize > num;
        }
        if (val.startsWith("<")) {
          const num = parseSize(val.slice(1));
          return t.fileSize < num;
        }
        if (val.includes("..")) {
          const parts = val.split("..").map(parseSize);
          const min = parts[0] ?? 0;
          const max = parts[1] ?? 0;
          return t.fileSize >= min && t.fileSize <= max;
        }
        return t.fileSize === parseSize(val);
      }
      if (filter.type === "speed") {
        const val = filter.value.toLowerCase();
        if (val.startsWith(">")) {
          const num = parseSize(val.slice(1));
          return t.speed > num;
        }
        if (val.startsWith("<")) {
          const num = parseSize(val.slice(1));
          return t.speed < num;
        }
        return t.speed === parseSize(val);
      }
      if (filter.type === "name") {
        return t.fileName.toLowerCase().includes(filter.value.toLowerCase());
      }
      return true;
    });
  }

  if (textQuery) {
    result = result.filter((t) =>
      t.fileName.toLowerCase().includes(textQuery.toLowerCase()),
    );
  }

  return result;
}

const filteredTasks = createRootMemo(() => {
  return computeFilteredTasks(
    $tasks.get(),
    sidebarFilter(),
    fileTypeFilter(),
    searchFilters().filters,
    searchFilters().textQuery,
  );
});

const taskCounts = createRootMemo((): TaskCounts => {
  const tasks = $tasks.get();
  let downloading = 0;
  let completed = 0;
  let paused = 0;
  let failed = 0;

  for (const task of tasks) {
    if (
      task.status === "downloading" ||
      task.status === "connecting" ||
      task.status === "resuming" ||
      task.status === "verifying"
    )
      downloading++;
    else if (task.status === "completed") completed++;
    else if (task.status === "paused") paused++;
    else if (task.status === "failed") failed++;
  }

  return { all: tasks.length, downloading, completed, paused, failed };
});

const fileTypeCounts = createRootMemo(() => {
  const tasks = $tasks.get();
  const counts: Record<FileTypeFilter, number> = {
    all: tasks.length,
    video: 0,
    audio: 0,
    document: 0,
    image: 0,
    archive: 0,
    executable: 0,
    model: 0,
    other: 0,
  };

  for (const task of tasks) {
    const type = getFileTypeByName(task.fileName) as FileTypeFilter;
    if (counts[type] !== undefined) {
      counts[type]++;
    } else {
      counts.other++;
    }
  }

  return counts;
});

export function removeSearchFilter(raw: string): void {
  setSearchQuery((q) => q.replace(raw, "").trim().replace(/\s+/g, " "));
}

export function resetFilters(): void {
  setSearchQuery("");
  setSidebarFilter("all");
  setFileTypeFilter("all");
}

/** toggle 筛选：点已选中的分类再点一次 = 回到 'all'（可取消，避免点了看不到全部）。 */
export function toggleSidebarFilter(key: SidebarFilter): void {
  setSidebarFilter((prev) => (prev === key ? "all" : key));
}

/** toggle 文件类型筛选：同上，再点取消回 'all'。 */
export function toggleFileTypeFilter(key: FileTypeFilter): void {
  setFileTypeFilter((prev) => (prev === key ? "all" : key));
}

export const $taskFilter = {
  get searchQuery(): Accessor<string> {
    return searchQuery;
  },
  get sidebarFilter(): Accessor<SidebarFilter> {
    return sidebarFilter;
  },
  get fileTypeFilter(): Accessor<FileTypeFilter> {
    return fileTypeFilter;
  },
  get sortState(): Accessor<SortState> {
    return sortState;
  },
  get searchFilters(): Accessor<{
    filters: { type: string; value: string; raw: string }[];
    textQuery: string;
  }> {
    return searchFilters;
  },
  get filteredTasks(): Accessor<TaskInfo[]> {
    return filteredTasks;
  },
  get taskCounts(): Accessor<TaskCounts> {
    return taskCounts;
  },
  get fileTypeCounts(): Accessor<Record<FileTypeFilter, number>> {
    return fileTypeCounts;
  },
};

export {
  setSearchQuery,
  setSidebarFilter,
  setFileTypeFilter,
  searchQuery,
  sidebarFilter,
  fileTypeFilter,
  sortState,
  setSortState,
};

// 在 createRoot 下读取状态，避免测试环境出现 computations created outside a createRoot 警告
export function readTaskFilterState(): FilterState {
  return createRoot((dispose) => {
    try {
      return {
        searchQuery: searchQuery(),
        sidebarFilter: sidebarFilter(),
        fileTypeFilter: fileTypeFilter(),
      };
    } finally {
      dispose();
    }
  });
}

// 模块级 createEffect 监听状态变化并持久化。
// createRoot 提供 reactive owner，避免测试环境出现 warnings。
createRoot(() => {
  createEffect(() => {
    save({
      searchQuery: searchQuery(),
      sidebarFilter: sidebarFilter(),
      fileTypeFilter: fileTypeFilter(),
      sort: sortState(),
    });
  });
});
