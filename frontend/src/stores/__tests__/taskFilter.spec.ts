import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { createRoot } from "solid-js";
import type { TaskInfo } from "../../types";

const mockGetTaskList = vi.fn();
const mockAddToast = vi.fn();

vi.mock("../../api/invoke", () => ({
  api: {
    getTaskList: (...args: unknown[]) => mockGetTaskList(...args),
  },
}));

vi.mock("../toast", () => ({
  addToast: (...args: unknown[]) => mockAddToast(...args),
}));

const makeTask = (id: string, overrides: Partial<TaskInfo> = {}): TaskInfo => ({
  id,
  url: `https://example.com/${id}.bin`,
  fileName: `${id}.bin`,
  fileSize: 1048576,
  downloaded: 0,
  speed: 0,
  status: "downloading",
  progress: 0.5,
  fragmentsTotal: 4,
  fragmentsDone: 2,
  createdAt: "2026-05-30T00:00:00Z",
  savePath: "/downloads",
  ...overrides,
});

let downloadsModule: typeof import("../downloads");
let taskFilterModule: typeof import("../taskFilter");

function read<T>(fn: () => T): T {
  return createRoot((dispose) => {
    try {
      return fn();
    } finally {
      dispose();
    }
  });
}

beforeEach(async () => {
  vi.resetModules();
  localStorage.clear();
  mockGetTaskList.mockReset();
  mockAddToast.mockReset();
  downloadsModule = await import("../downloads");
  taskFilterModule = await import("../taskFilter");
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("taskFilter store", () => {
  it("默认过滤器为空", () => {
    const state = taskFilterModule.readTaskFilterState();
    expect(state.searchQuery).toBe("");
    expect(state.sidebarFilter).toBe("all");
    expect(state.fileTypeFilter).toBe("all");
  });

  it("setSearchQuery 更新搜索词", () => {
    taskFilterModule.setSearchQuery("status:completed name:report");
    expect(read(() => taskFilterModule.$taskFilter.searchQuery())).toBe(
      "status:completed name:report",
    );
  });

  it("searchFilters 解析 status / type / size / speed / name 语法", () => {
    taskFilterModule.setSearchQuery(
      "status:completed type:video size:>10mb speed:>1mb name:report",
    );
    const sf = read(() => taskFilterModule.$taskFilter.searchFilters());
    expect(sf.filters).toHaveLength(5);
    expect(sf.filters.map((f) => f.type)).toEqual([
      "status",
      "type",
      "size",
      "speed",
      "name",
    ]);
    expect(sf.textQuery).toBe("");
  });

  it("searchFilters 提取普通文本查询", () => {
    taskFilterModule.setSearchQuery("status:completed report");
    const sf = read(() => taskFilterModule.$taskFilter.searchFilters());
    expect(sf.textQuery).toBe("report");
  });

  it("removeSearchFilter 移除指定 filter 原始文本", () => {
    taskFilterModule.setSearchQuery("status:completed name:report");
    taskFilterModule.removeSearchFilter("status:completed");
    expect(read(() => taskFilterModule.$taskFilter.searchQuery())).toBe(
      "name:report",
    );
  });

  it("sidebarFilter 按状态过滤任务", () => {
    downloadsModule.setTasks([
      makeTask("t1", { status: "downloading" }),
      makeTask("t2", { status: "completed" }),
      makeTask("t3", { status: "paused" }),
      makeTask("t4", { status: "failed" }),
    ]);

    taskFilterModule.setSidebarFilter("completed");
    expect(
      read(() => taskFilterModule.$taskFilter.filteredTasks()).map((t) => t.id),
    ).toEqual(["t2"]);

    taskFilterModule.setSidebarFilter("downloading");
    expect(
      read(() => taskFilterModule.$taskFilter.filteredTasks()).map((t) => t.id),
    ).toEqual(["t1"]);

    taskFilterModule.setSidebarFilter("paused");
    expect(
      read(() => taskFilterModule.$taskFilter.filteredTasks()).map((t) => t.id),
    ).toEqual(["t3"]);

    taskFilterModule.setSidebarFilter("failed");
    expect(
      read(() => taskFilterModule.$taskFilter.filteredTasks()).map((t) => t.id),
    ).toEqual(["t4"]);
  });

  it("fileTypeFilter 按文件类型过滤任务", () => {
    downloadsModule.setTasks([
      makeTask("t1", { fileName: "movie.mp4" }),
      makeTask("t2", { fileName: "song.mp3" }),
      makeTask("t3", { fileName: "doc.pdf" }),
      makeTask("t4", { fileName: "image.png" }),
      makeTask("t5", { fileName: "archive.zip" }),
      makeTask("t6", { fileName: "app.exe" }),
      makeTask("t7", { fileName: "unknown.xyz" }),
    ]);

    taskFilterModule.setFileTypeFilter("video");
    expect(
      read(() => taskFilterModule.$taskFilter.filteredTasks()).map((t) => t.id),
    ).toEqual(["t1"]);

    taskFilterModule.setFileTypeFilter("audio");
    expect(
      read(() => taskFilterModule.$taskFilter.filteredTasks()).map((t) => t.id),
    ).toEqual(["t2"]);

    taskFilterModule.setFileTypeFilter("document");
    expect(
      read(() => taskFilterModule.$taskFilter.filteredTasks()).map((t) => t.id),
    ).toEqual(["t3"]);

    taskFilterModule.setFileTypeFilter("other");
    expect(
      read(() => taskFilterModule.$taskFilter.filteredTasks()).map((t) => t.id),
    ).toEqual(["t7"]);
  });

  it("searchFilters 按文件大小过滤", () => {
    downloadsModule.setTasks([
      makeTask("t1", { fileSize: 5 * 1024 * 1024 }),
      makeTask("t2", { fileSize: 15 * 1024 * 1024 }),
      makeTask("t3", { fileSize: 25 * 1024 * 1024 }),
    ]);

    taskFilterModule.setSearchQuery("size:>10mb");
    expect(
      read(() => taskFilterModule.$taskFilter.filteredTasks()).map((t) => t.id),
    ).toEqual(["t2", "t3"]);

    taskFilterModule.setSearchQuery("size:5mb..20mb");
    expect(
      read(() => taskFilterModule.$taskFilter.filteredTasks()).map((t) => t.id),
    ).toEqual(["t1", "t2"]);
  });

  it("searchFilters 按速度过滤", () => {
    downloadsModule.setTasks([
      makeTask("t1", { speed: 1024 }),
      makeTask("t2", { speed: 2 * 1024 * 1024 }),
      makeTask("t3", { speed: 10 * 1024 * 1024 }),
    ]);

    taskFilterModule.setSearchQuery("speed:>1mb");
    expect(
      read(() => taskFilterModule.$taskFilter.filteredTasks()).map((t) => t.id),
    ).toEqual(["t2", "t3"]);
  });

  it("searchFilters 按文件名文本过滤", () => {
    downloadsModule.setTasks([
      makeTask("t1", { fileName: "report.pdf" }),
      makeTask("t2", { fileName: "summary.pdf" }),
      makeTask("t3", { fileName: "notes.txt" }),
    ]);

    taskFilterModule.setSearchQuery("report");
    expect(
      read(() => taskFilterModule.$taskFilter.filteredTasks()).map((t) => t.id),
    ).toEqual(["t1"]);
  });

  it("taskCounts 返回各类状态计数", () => {
    downloadsModule.setTasks([
      makeTask("t1", { status: "downloading" }),
      makeTask("t2", { status: "connecting" }),
      makeTask("t3", { status: "completed" }),
      makeTask("t4", { status: "paused" }),
      makeTask("t5", { status: "failed" }),
    ]);

    const counts = read(() => taskFilterModule.$taskFilter.taskCounts());
    expect(counts.all).toBe(5);
    expect(counts.downloading).toBe(2);
    expect(counts.completed).toBe(1);
    expect(counts.paused).toBe(1);
    expect(counts.failed).toBe(1);
  });

  it("fileTypeCounts 返回各文件类型计数", () => {
    downloadsModule.setTasks([
      makeTask("t1", { fileName: "a.mp4" }),
      makeTask("t2", { fileName: "b.mp3" }),
      makeTask("t3", { fileName: "c.pdf" }),
      makeTask("t4", { fileName: "d.png" }),
      makeTask("t5", { fileName: "e.zip" }),
      makeTask("t6", { fileName: "f.exe" }),
      makeTask("t7", { fileName: "g.xyz" }),
    ]);

    const counts = read(() => taskFilterModule.$taskFilter.fileTypeCounts());
    expect(counts.all).toBe(7);
    expect(counts.video).toBe(1);
    expect(counts.audio).toBe(1);
    expect(counts.document).toBe(1);
    expect(counts.image).toBe(1);
    expect(counts.archive).toBe(1);
    expect(counts.executable).toBe(1);
    expect(counts.other).toBe(1);
  });

  it("resetFilters 重置所有过滤器", () => {
    taskFilterModule.setSearchQuery("status:completed");
    taskFilterModule.setSidebarFilter("completed");
    taskFilterModule.setFileTypeFilter("video");

    taskFilterModule.resetFilters();
    const state = taskFilterModule.readTaskFilterState();
    expect(state.searchQuery).toBe("");
    expect(state.sidebarFilter).toBe("all");
    expect(state.fileTypeFilter).toBe("all");
  });
});

describe("taskFilter view persistence", () => {
  const STORAGE_KEY = "tachyon.tasklist.view";

  async function loadStore() {
    vi.resetModules();
    return import("../taskFilter");
  }

  it("默认视图状态并持久化到 localStorage", async () => {
    const mod = await loadStore();
    const state = mod.readTaskFilterState();
    expect(state).toEqual({
      searchQuery: "",
      sidebarFilter: "all",
      fileTypeFilter: "all",
    });
    expect(read(() => mod.sortState())).toEqual({ key: null, dir: "desc" });

    const saved = JSON.parse(localStorage.getItem(STORAGE_KEY)!);
    expect(saved).toMatchObject({
      searchQuery: "",
      sidebarFilter: "all",
      fileTypeFilter: "all",
      sort: { key: null, dir: "desc" },
    });
  });

  it("读取 localStorage 中的合法视图值", async () => {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        searchQuery: "report",
        sidebarFilter: "completed",
        fileTypeFilter: "video",
        sort: { key: "speed", dir: "asc" },
      }),
    );
    const mod = await loadStore();
    const state = mod.readTaskFilterState();
    expect(state).toEqual({
      searchQuery: "report",
      sidebarFilter: "completed",
      fileTypeFilter: "video",
    });
    expect(read(() => mod.sortState())).toEqual({ key: "speed", dir: "asc" });
  });

  it("非法 localStorage 值回退到默认值", async () => {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        searchQuery: 123,
        sidebarFilter: "done",
        fileTypeFilter: "binary",
        sort: { key: "priority", dir: "up" },
      }),
    );
    const mod = await loadStore();
    const state = mod.readTaskFilterState();
    expect(state).toEqual({
      searchQuery: "",
      sidebarFilter: "all",
      fileTypeFilter: "all",
    });
    expect(read(() => mod.sortState())).toEqual({ key: null, dir: "desc" });
  });

  it("损坏的 JSON 回退到默认值", async () => {
    localStorage.setItem(STORAGE_KEY, "{not json");
    const mod = await loadStore();
    expect(read(() => mod.sortState())).toEqual({ key: null, dir: "desc" });
    expect(mod.readTaskFilterState().searchQuery).toBe("");
  });

  it("localStorage 读取异常时回退到默认值", async () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("storage disabled");
    });
    const mod = await loadStore();
    expect(read(() => mod.sortState())).toEqual({ key: null, dir: "desc" });
    expect(mod.readTaskFilterState().searchQuery).toBe("");
  });

  it("修改搜索词后持久化", async () => {
    const mod = await loadStore();
    mod.setSearchQuery("hello");
    const saved = JSON.parse(localStorage.getItem(STORAGE_KEY)!);
    expect(saved.searchQuery).toBe("hello");
    expect(saved.sidebarFilter).toBe("all");
    expect(saved.sort).toEqual({ key: null, dir: "desc" });
  });

  it("修改 sidebarFilter 后持久化", async () => {
    const mod = await loadStore();
    mod.setSidebarFilter("failed");
    const saved = JSON.parse(localStorage.getItem(STORAGE_KEY)!);
    expect(saved.sidebarFilter).toBe("failed");
  });

  it("修改 fileTypeFilter 后持久化", async () => {
    const mod = await loadStore();
    mod.setFileTypeFilter("archive");
    const saved = JSON.parse(localStorage.getItem(STORAGE_KEY)!);
    expect(saved.fileTypeFilter).toBe("archive");
  });

  it("修改排序状态后持久化", async () => {
    const mod = await loadStore();
    mod.setSortState({ key: "progress", dir: "desc" });
    expect(read(() => mod.sortState())).toEqual({ key: "progress", dir: "desc" });
    const saved = JSON.parse(localStorage.getItem(STORAGE_KEY)!);
    expect(saved.sort).toEqual({ key: "progress", dir: "desc" });
  });

  it("resetFilters 重置过滤器但不重置排序", async () => {
    const mod = await loadStore();
    mod.setSearchQuery("foo");
    mod.setSidebarFilter("paused");
    mod.setSortState({ key: "speed", dir: "asc" });
    mod.resetFilters();
    expect(mod.readTaskFilterState()).toEqual({
      searchQuery: "",
      sidebarFilter: "all",
      fileTypeFilter: "all",
    });
    expect(read(() => mod.sortState())).toEqual({ key: "speed", dir: "asc" });
  });

  it("localStorage 写入异常时不抛错", async () => {
    const mod = await loadStore();
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("storage full");
    });
    expect(() => mod.setSearchQuery("boom")).not.toThrow();
  });
});
