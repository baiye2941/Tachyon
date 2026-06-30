import { createSignal, createRoot, type Accessor } from "solid-js";
import type { TaskInfo, SidebarFilter, FileTypeFilter } from "../types";
import { $tasks } from "./downloads";
import { EXT_TYPE_MAP } from "../utils/format";
import { createRootMemo } from "../utils/reactive";

export interface SearchFilter {
  type: string;
  value: string;
  raw: string;
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

const [searchQuery, setSearchQuery] = createSignal("");
const [sidebarFilter, setSidebarFilter] = createSignal<SidebarFilter>("all");
const [fileTypeFilter, setFileTypeFilter] = createSignal<FileTypeFilter>("all");

const searchFilters = createRootMemo(() => {
  const query = searchQuery().trim();
  const filters: SearchFilter[] = [];
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
  filters: SearchFilter[],
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

/** toggle 筛选:点已选中的分类再点一次 = 回到 'all'(可取消,避免点了看不到全部)。 */
export function toggleSidebarFilter(key: SidebarFilter): void {
  setSidebarFilter((prev) => (prev === key ? "all" : key));
}

/** toggle 文件类型筛选:同上,再点取消回 'all'。 */
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
  get searchFilters(): Accessor<{
    filters: SearchFilter[];
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
