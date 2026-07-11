import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

describe("detailPanel store", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.resetModules();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  async function loadStore() {
    const mod = await import("../detailPanel");
    return mod;
  }

  it("默认宽度为 360", async () => {
    const { $detailPanel } = await loadStore();
    expect($detailPanel.width()).toBe(360);
  });

  it("读取 localStorage 中的合法宽度值", async () => {
    localStorage.setItem("tachyon.detailPanel.width", JSON.stringify(420));
    const { $detailPanel } = await loadStore();
    expect($detailPanel.width()).toBe(420);
  });

  it("非数字 localStorage 值回退到默认值", async () => {
    localStorage.setItem("tachyon.detailPanel.width", JSON.stringify("wide"));
    const { $detailPanel } = await loadStore();
    expect($detailPanel.width()).toBe(360);
  });

  it("越界 localStorage 值回退到默认值", async () => {
    localStorage.setItem("tachyon.detailPanel.width", JSON.stringify(700));
    const { $detailPanel } = await loadStore();
    expect($detailPanel.width()).toBe(360);
  });

  it("损坏的 JSON 回退到默认值", async () => {
    localStorage.setItem("tachyon.detailPanel.width", "{not json");
    const { $detailPanel } = await loadStore();
    expect($detailPanel.width()).toBe(360);
  });

  it("localStorage 读取异常时回退到默认值", async () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("storage disabled");
    });
    const { $detailPanel } = await loadStore();
    expect($detailPanel.width()).toBe(360);
  });

  it("setWidth 在合法范围内更新并持久化", async () => {
    const { $detailPanel, setWidth } = await loadStore();
    setWidth(420);
    expect($detailPanel.width()).toBe(420);
    expect(localStorage.getItem("tachyon.detailPanel.width")).toBe(
      JSON.stringify(420),
    );
  });

  it("setWidth 对低于最小值的输入做 clamp", async () => {
    const { $detailPanel, setWidth } = await loadStore();
    setWidth(100);
    expect($detailPanel.width()).toBe(280);
    expect(localStorage.getItem("tachyon.detailPanel.width")).toBe(
      JSON.stringify(280),
    );
  });

  it("setWidth 对高于最大值的输入做 clamp", async () => {
    const { $detailPanel, setWidth } = await loadStore();
    setWidth(900);
    expect($detailPanel.width()).toBe(600);
    expect(localStorage.getItem("tachyon.detailPanel.width")).toBe(
      JSON.stringify(600),
    );
  });

  it("setWidth 对非有限数不做任何更改", async () => {
    const { $detailPanel, setWidth } = await loadStore();
    setWidth(NaN);
    expect($detailPanel.width()).toBe(360);
  });

  it("resetWidth 恢复默认值并持久化", async () => {
    const { $detailPanel, setWidth, resetWidth } = await loadStore();
    setWidth(500);
    resetWidth();
    expect($detailPanel.width()).toBe(360);
    expect(localStorage.getItem("tachyon.detailPanel.width")).toBe(
      JSON.stringify(360),
    );
  });

  it("localStorage 写入异常时不抛错", async () => {
    const { setWidth } = await loadStore();
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("storage full");
    });
    expect(() => setWidth(400)).not.toThrow();
  });
});
