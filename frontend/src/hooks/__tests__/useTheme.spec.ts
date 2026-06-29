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
});
