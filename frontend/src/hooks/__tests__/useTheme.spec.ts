import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { renderHook } from "@solidjs/testing-library";

const THEME_KEY = "tachyon-theme";

async function loadUseTheme() {
  vi.resetModules();
  return (await import("../useTheme")).useTheme;
}

describe("useTheme", () => {
  beforeEach(() => {
    localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("初始 theme 为 dark", async () => {
    const useTheme = await loadUseTheme();
    const { result } = renderHook(useTheme);
    expect(result.theme()).toBe("dark");
  });

  it('setTheme("light") 更新 signal、写 localStorage、设置 data-theme', async () => {
    const useTheme = await loadUseTheme();
    const { result } = renderHook(useTheme);

    result.setTheme("light");

    expect(result.theme()).toBe("light");
    expect(localStorage.getItem(THEME_KEY)).toBe("light");
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
  });

  it("toggleTheme 从 dark 切到 light 再切回 dark", async () => {
    const useTheme = await loadUseTheme();
    const { result } = renderHook(useTheme);

    expect(result.theme()).toBe("dark");

    result.toggleTheme();
    expect(result.theme()).toBe("light");
    expect(localStorage.getItem(THEME_KEY)).toBe("light");
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");

    result.toggleTheme();
    expect(result.theme()).toBe("dark");
    expect(localStorage.getItem(THEME_KEY)).toBe("dark");
    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
  });

  it("localStorage.setItem 抛错时不崩溃(静默降级)", async () => {
    const useTheme = await loadUseTheme();
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("QuotaExceeded");
    });

    const { result } = renderHook(useTheme);

    expect(() => result.setTheme("light")).not.toThrow();
    // signal 仍应更新(降级仅影响持久化)
    expect(result.theme()).toBe("light");
    // data-theme 仍应设置
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
  });

  it("无 startViewTransition 时带 origin 也走即时降级", async () => {
    const useTheme = await loadUseTheme();
    const { result } = renderHook(useTheme);

    result.toggleTheme({ x: 100, y: 200 });

    expect(result.theme()).toBe("light");
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
  });

  it("支持 View Transitions 时对 ::view-transition-new(root) 做圆形扩散", async () => {
    const useTheme = await loadUseTheme();
    const animateMock = vi.fn();
    // lib.dom 已内置 startViewTransition 类型,此处用 as unknown as 整体替换,
    // 避免与内置签名交叉冲突;delete 要求属性可选,故用独立结构类型
    const doc = document as unknown as {
      startViewTransition?: (cb: () => void) => {
        ready: Promise<void>;
        finished: Promise<void>;
        skipTransition: () => void;
      };
    };
    const root = document.documentElement as unknown as {
      animate?: (keyframes: unknown, options: unknown) => void;
    };
    const prevAnimate = root.animate;
    root.animate = animateMock;
    doc.startViewTransition = (cb: () => void) => {
      cb();
      return {
        ready: Promise.resolve(),
        finished: Promise.resolve(),
        skipTransition: vi.fn(),
      };
    };

    try {
      const { result } = renderHook(useTheme);
      result.toggleTheme({ x: 10, y: 20 });

      // 主题在 transition 回调内同步应用
      expect(document.documentElement.getAttribute("data-theme")).toBe("light");

      await vi.waitFor(() => expect(animateMock).toHaveBeenCalled());
      const options = animateMock.mock.calls[0]?.[1] as {
        pseudoElement?: string;
      };
      expect(options.pseudoElement).toBe("::view-transition-new(root)");
    } finally {
      delete doc.startViewTransition;
      if (prevAnimate) {
        root.animate = prevAnimate;
      } else {
        delete root.animate;
      }
    }
  });
});
