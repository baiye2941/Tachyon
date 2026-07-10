import { describe, it, expect, beforeEach, vi } from "vitest";

const RECENT_KEY = "tachyon.cmd.recent";
const PINNED_KEY = "tachyon.cmd.pinned";

interface CommandHistoryStore {
  recent: string[];
  pinned: string[];
  addRecentCommand: (id: string) => void;
  togglePinnedCommand: (id: string) => void;
  isPinned: (id: string) => boolean;
}

let store: CommandHistoryStore;

beforeEach(async () => {
  localStorage.clear();
  vi.resetModules();
  const mod = await import("../commandHistory");
  store = {
    recent: mod.$recent,
    pinned: mod.$pinned,
    addRecentCommand: mod.addRecentCommand,
    togglePinnedCommand: mod.togglePinnedCommand,
    isPinned: mod.isPinned,
  };
});

describe("CommandHistoryStore", () => {
  it("初始状态为空数组", () => {
    expect(store.recent).toEqual([]);
    expect(store.pinned).toEqual([]);
  });

  it("addRecentCommand 去重并将最新项排到最前", () => {
    store.addRecentCommand("nav-downloads");
    store.addRecentCommand("nav-sniffer");
    store.addRecentCommand("nav-downloads");

    expect(store.recent).toEqual(["nav-downloads", "nav-sniffer"]);
  });

  it("addRecentCommand 超过 8 条时截断", () => {
    const ids = [
      "nav-downloads",
      "nav-sniffer",
      "nav-history",
      "nav-stats",
      "nav-settings",
      "task-new",
      "act-pause-all",
      "act-resume-all",
      "act-cancel-all",
    ];
    ids.forEach((id) => store.addRecentCommand(id));

    expect(store.recent).toHaveLength(8);
    expect(store.recent[0]).toBe("act-cancel-all");
    expect(store.recent[7]).toBe("nav-sniffer");
  });

  it("addRecentCommand 忽略不存在的命令 id", () => {
    store.addRecentCommand("not-a-command");
    expect(store.recent).toEqual([]);
  });

  it("togglePinnedCommand 切换置顶状态", () => {
    store.togglePinnedCommand("nav-settings");
    expect(store.pinned).toEqual(["nav-settings"]);
    expect(store.isPinned("nav-settings")).toBe(true);

    store.togglePinnedCommand("nav-settings");
    expect(store.pinned).toEqual([]);
    expect(store.isPinned("nav-settings")).toBe(false);
  });

  it("togglePinnedCommand 忽略不存在的命令 id", () => {
    store.togglePinnedCommand("not-a-command");
    expect(store.pinned).toEqual([]);
  });

  it("recent 与 pinned 分别持久化到 localStorage", () => {
    store.addRecentCommand("task-new");
    store.togglePinnedCommand("nav-settings");

    expect(JSON.parse(localStorage.getItem(RECENT_KEY)!)).toEqual(["task-new"]);
    expect(JSON.parse(localStorage.getItem(PINNED_KEY)!)).toEqual([
      "nav-settings",
    ]);
  });

  it("加载时过滤掉 COMMANDS 中不存在的 id", async () => {
    localStorage.setItem(RECENT_KEY, JSON.stringify(["nav-downloads", "old-cmd"]));
    localStorage.setItem(PINNED_KEY, JSON.stringify(["removed-cmd", "nav-settings"]));

    vi.resetModules();
    const mod = await import("../commandHistory");
    expect(mod.$recent).toEqual(["nav-downloads"]);
    expect(mod.$pinned).toEqual(["nav-settings"]);
  });

  it("localStorage 损坏时回退到空数组", async () => {
    localStorage.setItem(RECENT_KEY, "invalid json{[");
    localStorage.setItem(PINNED_KEY, "not-json");

    vi.resetModules();
    const mod = await import("../commandHistory");
    expect(mod.$recent).toEqual([]);
    expect(mod.$pinned).toEqual([]);
  });

  it("localStorage setItem 异常时不崩溃", () => {
    const originalSetItem = localStorage.setItem;
    localStorage.setItem = vi.fn(() => {
      throw new Error("QuotaExceeded");
    });

    expect(() => store.addRecentCommand("nav-downloads")).not.toThrow();
    expect(() => store.togglePinnedCommand("nav-settings")).not.toThrow();

    localStorage.setItem = originalSetItem;
  });
});
