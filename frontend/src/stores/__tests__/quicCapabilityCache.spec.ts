import { describe, it, expect, vi } from "vitest";
import { createRoot } from "solid-js";

vi.mock("../../api/invoke", () => ({
  api: {
    getQuicCapability: vi
      .fn()
      .mockResolvedValue({ enableQuic: true, effectiveQuic: true }),
  },
}));

describe("quicCapabilityCache", () => {
  it("模块级单例:多次调用返回同一 resource,fetch 只发生一次", async () => {
    // 单例存于模块状态,重置模块以隔离其他用例/先前挂载的影响
    vi.resetModules();
    const { getQuicCapabilityResource } = await import(
      "../quicCapabilityCache"
    );
    const { api } = await import("../../api/invoke");
    createRoot((dispose) => {
      const r1 = getQuicCapabilityResource();
      const r2 = getQuicCapabilityResource();
      expect(r1).toBe(r2);
      dispose();
    });
    // 等 fetch 微任务结算后断言只取一次
    await new Promise((r) => setTimeout(r, 0));
    expect(api.getQuicCapability).toHaveBeenCalledTimes(1);
  });
});
