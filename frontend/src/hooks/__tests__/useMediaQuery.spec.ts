import { describe, it, expect, vi, afterEach, beforeEach } from "vitest";
import { renderHook } from "@solidjs/testing-library";
import {
  useMediaQuery,
  useIsNarrowScreen,
  useIsSmallScreen,
  BREAKPOINTS,
} from "../useMediaQuery";

// matchMedia mock
function mockMatchMedia(matches: boolean) {
  const listeners: ((e: MediaQueryListEvent) => void)[] = [];
  const mql = {
    matches,
    media: "",
    onchange: null,
    addEventListener: (
      _type: string,
      listener: (e: MediaQueryListEvent) => void,
    ) => listeners.push(listener),
    removeEventListener: (
      _type: string,
      listener: (e: MediaQueryListEvent) => void,
    ) => {
      const i = listeners.indexOf(listener);
      if (i >= 0) listeners.splice(i, 1);
    },
    dispatchEvent: () => true,
    addListener: () => {},
    removeListener: () => {},
  };
  vi.stubGlobal("matchMedia", () => mql);
  return { mql, listeners };
}

describe("useMediaQuery", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("返回初始匹配状态(false)", () => {
    mockMatchMedia(false);
    const { result } = renderHook(() => useMediaQuery("(max-width: 768px)"));
    expect(result()).toBe(false);
  });

  it("返回初始匹配状态(true)", () => {
    mockMatchMedia(true);
    const { result } = renderHook(() => useMediaQuery("(max-width: 768px)"));
    expect(result()).toBe(true);
  });

  it("断点常量已定义", () => {
    expect(BREAKPOINTS.sm).toBe(640);
    expect(BREAKPOINTS.md).toBe(768);
    expect(BREAKPOINTS.lg).toBe(1024);
    expect(BREAKPOINTS.xl).toBe(1280);
  });

  it("useIsNarrowScreen 使用 md 断点", () => {
    mockMatchMedia(true);
    const { result } = renderHook(() => useIsNarrowScreen());
    expect(result()).toBe(true);
  });

  it("useIsSmallScreen 使用 sm 断点", () => {
    mockMatchMedia(true);
    const { result } = renderHook(() => useIsSmallScreen());
    expect(result()).toBe(true);
  });
});
