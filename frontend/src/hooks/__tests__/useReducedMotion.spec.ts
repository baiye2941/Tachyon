import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createRoot } from "solid-js";
import { useReducedMotion } from "../useReducedMotion";

describe("useReducedMotion", () => {
  beforeEach(() => {
    vi.stubGlobal(
      "matchMedia",
      vi.fn().mockImplementation((query: string) => ({
        matches: query === "(prefers-reduced-motion: reduce)",
        media: query,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
      })),
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("matchMedia 不支持时应返回 false getter", () =>
    createRoot((dispose) => {
      vi.stubGlobal("matchMedia", undefined);
      const reduced = useReducedMotion();
      expect(reduced()).toBe(false);
      dispose();
    }));

  it("prefers-reduced-motion: reduce 时应返回 true getter", () =>
    createRoot((dispose) => {
      const reduced = useReducedMotion();
      expect(reduced()).toBe(true);
      dispose();
    }));

  it("变化时应更新 getter 返回值", () =>
    createRoot((dispose) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      let changeHandler: any = null;
      vi.stubGlobal(
        "matchMedia",
        vi.fn().mockImplementation(() => ({
          matches: false,
          addEventListener: (_event: string, handler: (e: MediaQueryListEvent) => void) => {
            changeHandler = handler;
          },
          removeEventListener: vi.fn(),
        })),
      );

      const reduced = useReducedMotion();
      expect(reduced()).toBe(false);

      // 直接调用收集到的 handler 模拟媒体查询变化
      changeHandler?.({ matches: true });
      expect(reduced()).toBe(true);

      dispose();
    }));
});
