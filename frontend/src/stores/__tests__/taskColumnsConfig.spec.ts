import { describe, it, expect, beforeEach, vi } from "vitest";
import { createRoot } from "solid-js";

let columnsModule: typeof import("../taskColumnsConfig");

function read<T>(fn: () => T): T {
  return createRoot((dispose) => {
    try {
      return fn();
    } finally {
      dispose();
    }
  });
}

async function flushEffects(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

beforeEach(async () => {
  vi.resetModules();
  localStorage.clear();
  columnsModule = await import("../taskColumnsConfig");
});

describe("taskColumnsConfig store", () => {
  it("默认可见列为 name/progress/speed/status", () => {
    expect(read(() => columnsModule.$taskColumns.visibleKeys())).toEqual([
      "name",
      "progress",
      "speed",
      "status",
    ]);
  });

  it("默认宽度来自列定义", () => {
    expect(read(() => columnsModule.$taskColumns.width("name"))).toBe(
      "flex-1",
    );
    expect(read(() => columnsModule.$taskColumns.width("progress"))).toBe(120);
    expect(read(() => columnsModule.$taskColumns.width("speed"))).toBe(100);
    expect(read(() => columnsModule.$taskColumns.width("status"))).toBe(80);
  });

  it("name 不可隐藏", () => {
    columnsModule.$taskColumns.toggleVisibility("name");
    expect(read(() => columnsModule.$taskColumns.visibleKeys())).toContain(
      "name",
    );
  });

  it("toggleVisibility 可切换非 name 列", () => {
    columnsModule.$taskColumns.toggleVisibility("size");
    expect(read(() => columnsModule.$taskColumns.visibleKeys())).toContain(
      "size",
    );

    columnsModule.$taskColumns.toggleVisibility("size");
    expect(read(() => columnsModule.$taskColumns.visibleKeys())).not.toContain(
      "size",
    );
  });

  it("setWidth 限制最小宽度", () => {
    columnsModule.$taskColumns.setWidth("progress", 10);
    expect(read(() => columnsModule.$taskColumns.width("progress"))).toBe(60);

    columnsModule.$taskColumns.setWidth("progress", 200);
    expect(read(() => columnsModule.$taskColumns.width("progress"))).toBe(200);
  });

  it("resetColumns 恢复默认", () => {
    columnsModule.$taskColumns.toggleVisibility("size");
    columnsModule.$taskColumns.setWidth("progress", 200);
    columnsModule.$taskColumns.resetColumns();

    expect(read(() => columnsModule.$taskColumns.visibleKeys())).toEqual([
      "name",
      "progress",
      "speed",
      "status",
    ]);
    expect(read(() => columnsModule.$taskColumns.width("progress"))).toBe(120);
  });

  it("状态变化持久化到 localStorage", async () => {
    columnsModule.$taskColumns.toggleVisibility("size");
    columnsModule.$taskColumns.setWidth("progress", 200);
    await flushEffects();

    const raw = localStorage.getItem("tachyon.tasklist.columns");
    expect(raw).not.toBeNull();
    const parsed = JSON.parse(raw!);
    expect(parsed.version).toBe(1);
    expect(parsed.visible).toEqual([
      "name",
      "progress",
      "speed",
      "status",
      "size",
    ]);
    expect(parsed.widths).toEqual({ progress: 200 });
  });

  it("损坏的 localStorage 回退默认", async () => {
    localStorage.setItem("tachyon.tasklist.columns", "not-json");

    // 重新导入以触发 load
    vi.resetModules();
    columnsModule = await import("../taskColumnsConfig");

    expect(read(() => columnsModule.$taskColumns.visibleKeys())).toEqual([
      "name",
      "progress",
      "speed",
      "status",
    ]);
  });

  it("版本不匹配回退默认", async () => {
    localStorage.setItem(
      "tachyon.tasklist.columns",
      JSON.stringify({ version: 999, visible: ["name"], widths: {} }),
    );

    vi.resetModules();
    columnsModule = await import("../taskColumnsConfig");

    expect(read(() => columnsModule.$taskColumns.visibleKeys())).toEqual([
      "name",
      "progress",
      "speed",
      "status",
    ]);
  });

  it("非法 visible key 被过滤", async () => {
    localStorage.setItem(
      "tachyon.tasklist.columns",
      JSON.stringify({
        version: 1,
        visible: ["name", "progress", "invalidKey"],
        widths: {},
      }),
    );

    vi.resetModules();
    columnsModule = await import("../taskColumnsConfig");

    expect(read(() => columnsModule.$taskColumns.visibleKeys())).toEqual([
      "name",
      "progress",
    ]);
  });
});
