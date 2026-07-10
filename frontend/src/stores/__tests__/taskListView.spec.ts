import { describe, it, expect, beforeEach, vi } from "vitest";

describe("taskListView store", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.resetModules();
  });

  async function loadStore() {
    // 通过动态 import 确保每次测试都重新执行 load() 读取 localStorage
    const mod = await import("../taskListView");
    return mod;
  }

  it("默认值为 none", async () => {
    const { $taskListView } = await loadStore();
    expect($taskListView.groupBy()).toBe("none");
  });

  it("toggleGroupBy 在 none 与 status 之间切换", async () => {
    const { $taskListView, toggleGroupBy } = await loadStore();
    expect($taskListView.groupBy()).toBe("none");
    toggleGroupBy();
    expect($taskListView.groupBy()).toBe("status");
    toggleGroupBy();
    expect($taskListView.groupBy()).toBe("none");
  });

  it("setGroupBy 更新值", async () => {
    const { $taskListView, setGroupBy } = await loadStore();
    setGroupBy("status");
    expect($taskListView.groupBy()).toBe("status");
    setGroupBy("none");
    expect($taskListView.groupBy()).toBe("none");
  });

  it("非法 setGroupBy 被忽略", async () => {
    const { $taskListView, setGroupBy } = await loadStore();
    setGroupBy("status");
    // @ts-expect-error 故意传入非法值
    setGroupBy("byType");
    expect($taskListView.groupBy()).toBe("status");
  });

  it("变化时持久化到 localStorage", async () => {
    const { setGroupBy, toggleGroupBy } = await loadStore();
    setGroupBy("status");
    expect(localStorage.getItem("tachyon.tasklist.groupBy")).toBe(
      JSON.stringify("status"),
    );
    toggleGroupBy();
    expect(localStorage.getItem("tachyon.tasklist.groupBy")).toBe(
      JSON.stringify("none"),
    );
  });

  it("非法 localStorage 回退到 none", async () => {
    localStorage.setItem("tachyon.tasklist.groupBy", JSON.stringify("bad"));
    const { $taskListView } = await loadStore();
    expect($taskListView.groupBy()).toBe("none");
  });

  it("损坏的 localStorage 回退到 none", async () => {
    localStorage.setItem("tachyon.tasklist.groupBy", "{not json");
    const { $taskListView } = await loadStore();
    expect($taskListView.groupBy()).toBe("none");
  });
});
