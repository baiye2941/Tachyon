import { describe, it, expect, beforeEach, vi } from "vitest";

describe("onboarding store", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.resetModules();
  });

  async function loadStore() {
    const mod = await import("../onboarding");
    return mod;
  }

  it("首次使用默认未完成引导", async () => {
    const { $onboarding } = await loadStore();
    expect($onboarding.isCompleted()).toBe(false);
  });

  it("completeOnboarding 标记已完成", async () => {
    const { $onboarding, completeOnboarding } = await loadStore();
    completeOnboarding();
    expect($onboarding.isCompleted()).toBe(true);
  });

  it("resetOnboarding 重置为未完成", async () => {
    const { $onboarding, completeOnboarding, resetOnboarding } =
      await loadStore();
    completeOnboarding();
    resetOnboarding();
    expect($onboarding.isCompleted()).toBe(false);
  });

  it("完成后持久化到 localStorage", async () => {
    const { completeOnboarding } = await loadStore();
    completeOnboarding();
    expect(localStorage.getItem("tachyon.tasklist.onboarding.completed")).toBe(
      JSON.stringify(true),
    );
  });

  it("读取 localStorage 已完成状态", async () => {
    localStorage.setItem("tachyon.tasklist.onboarding.completed", "true");
    const { $onboarding } = await loadStore();
    expect($onboarding.isCompleted()).toBe(true);
  });

  it("非法 localStorage 值回退到未完成", async () => {
    localStorage.setItem("tachyon.tasklist.onboarding.completed", "invalid");
    const { $onboarding } = await loadStore();
    expect($onboarding.isCompleted()).toBe(false);
  });

  it("localStorage 读取异常时回退到未完成", async () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("storage disabled");
    });
    const { $onboarding } = await loadStore();
    expect($onboarding.isCompleted()).toBe(false);
  });

  it("localStorage 写入异常时不抛错", async () => {
    const { completeOnboarding } = await loadStore();
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("storage full");
    });
    expect(() => completeOnboarding()).not.toThrow();
  });
});
