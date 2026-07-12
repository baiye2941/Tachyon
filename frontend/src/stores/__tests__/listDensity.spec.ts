import { describe, it, expect, beforeEach, vi } from "vitest";

describe("listDensity store", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.resetModules();
  });

  async function loadStore() {
    // 通过动态 import 确保每次测试都重新执行 load() 读取 localStorage
    const mod = await import("../listDensity");
    return mod;
  }

  it("默认值为 comfortable", async () => {
    const { $listDensity } = await loadStore();
    expect($listDensity.density()).toBe("comfortable");
  });

  it("toggleListDensity 在 comfortable 与 compact 之间切换", async () => {
    const { $listDensity, toggleListDensity } = await loadStore();
    expect($listDensity.density()).toBe("comfortable");
    toggleListDensity();
    expect($listDensity.density()).toBe("compact");
    toggleListDensity();
    expect($listDensity.density()).toBe("comfortable");
  });

  it("setListDensity 更新为合法值", async () => {
    const { $listDensity, setListDensity } = await loadStore();
    setListDensity("compact");
    expect($listDensity.density()).toBe("compact");
    expect(localStorage.getItem("tachyon.tasklist.listDensity")).toBe(
      JSON.stringify("compact"),
    );
  });

  it("setListDensity 传入非法值时 signal 保持不变", async () => {
    const { $listDensity, setListDensity } = await loadStore();
    expect($listDensity.density()).toBe("comfortable");
    // @ts-expect-error 故意传入非法值
    setListDensity("cosy");
    expect($listDensity.density()).toBe("comfortable");
  });

  it("切换后持久化到 localStorage", async () => {
    const { toggleListDensity } = await loadStore();
    toggleListDensity();
    expect(localStorage.getItem("tachyon.tasklist.listDensity")).toBe(
      JSON.stringify("compact"),
    );
    toggleListDensity();
    expect(localStorage.getItem("tachyon.tasklist.listDensity")).toBe(
      JSON.stringify("comfortable"),
    );
  });

  it("读取合法 localStorage 值", async () => {
    localStorage.setItem("tachyon.tasklist.listDensity", JSON.stringify("compact"));
    const { $listDensity } = await loadStore();
    expect($listDensity.density()).toBe("compact");
  });

  it("非法 localStorage 回退到 comfortable", async () => {
    localStorage.setItem("tachyon.tasklist.listDensity", JSON.stringify("cosy"));
    const { $listDensity } = await loadStore();
    expect($listDensity.density()).toBe("comfortable");
  });

  it("损坏的 localStorage 回退到 comfortable", async () => {
    localStorage.setItem("tachyon.tasklist.listDensity", "{not json");
    const { $listDensity } = await loadStore();
    expect($listDensity.density()).toBe("comfortable");
  });

  it("localStorage 读取异常时回退到 comfortable", async () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("storage disabled");
    });
    const { $listDensity } = await loadStore();
    expect($listDensity.density()).toBe("comfortable");
  });

  it("localStorage 写入异常时不抛错", async () => {
    const { toggleListDensity } = await loadStore();
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("storage full");
    });
    expect(() => toggleListDensity()).not.toThrow();
  });
});
