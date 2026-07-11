import { describe, it, expect, beforeEach } from "vitest";
import * as selectionModule from "../../stores/selection";

describe("selection store", () => {
  beforeEach(() => {
    selectionModule.deselectAll();
  });

  it("初始状态为空选中集合", () => {
    expect(selectionModule.$selectedIds.get().size).toBe(0);
    expect(selectionModule.$selectedIds.get().has("any")).toBe(false);
  });

  it("toggleSelection 添加和移除任务ID", () => {
    selectionModule.toggleSelection("task-1");
    expect(selectionModule.$selectedIds.get().has("task-1")).toBe(true);

    selectionModule.toggleSelection("task-1");
    expect(selectionModule.$selectedIds.get().has("task-1")).toBe(false);
  });

  it("selectAll 选中所有任务ID", () => {
    selectionModule.selectAll(["a", "b", "c"]);
    expect(selectionModule.$selectedIds.get().size).toBe(3);
    expect(selectionModule.$selectedIds.get().has("a")).toBe(true);
    expect(selectionModule.$selectedIds.get().has("b")).toBe(true);
    expect(selectionModule.$selectedIds.get().has("c")).toBe(true);
  });

  it("deselectAll 清空选中", () => {
    selectionModule.selectAll(["a", "b"]);
    selectionModule.deselectAll();
    expect(selectionModule.$selectedIds.get().size).toBe(0);
  });

  it("isSelected 判断单个ID是否选中", () => {
    selectionModule.toggleSelection("task-1");
    expect(selectionModule.isSelected("task-1")).toBe(true);
    expect(selectionModule.isSelected("task-2")).toBe(false);
  });

  it("selectedCount 返回选中数量", () => {
    expect(selectionModule.selectedCount()).toBe(0);
    selectionModule.toggleSelection("a");
    expect(selectionModule.selectedCount()).toBe(1);
    selectionModule.toggleSelection("b");
    expect(selectionModule.selectedCount()).toBe(2);
  });

  it("hasSelection 返回是否有选中", () => {
    expect(selectionModule.hasSelection()).toBe(false);
    selectionModule.toggleSelection("x");
    expect(selectionModule.hasSelection()).toBe(true);
  });
});

