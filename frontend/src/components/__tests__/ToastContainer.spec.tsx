import { describe, it, expect, afterEach, beforeEach, vi } from "vitest";
import { render, fireEvent, cleanup } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import ToastContainer, {
  addToast,
  removeToast,
  getToasts,
} from "../ToastContainer";

// matchMedia stub:jsdom 无 matchMedia 实现,useReducedMotion 依赖它
// (stub 风格参考 NewTaskModal.spec.tsx;matches=false 表示不减少动画)
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
}

// 清空模块级 toast 状态,避免用例间串扰(排空模式参考 Announcer.spec.tsx)
function drainToasts() {
  getToasts().forEach((t) => removeToast(t.id));
}

describe("ToastContainer", () => {
  beforeEach(() => {
    mockMatchMedia(false);
    vi.useFakeTimers();
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    drainToasts();
    vi.unstubAllGlobals();
  });

  it("keyframes.css 定义 toast-in/toast-out 关键帧(toast.css 引用)", () => {
    // vitest cwd = frontend,用 node fs 读 CSS 文本断言
    const css = readFileSync(
      join(process.cwd(), "src/styles/keyframes.css"),
      "utf-8",
    );
    expect(css).toContain("@keyframes toast-in");
    expect(css).toContain("@keyframes toast-out");
    // 零引用死 keyframes logo-shimmer 已删除(grep 全项目仅定义处出现,无任何引用)
    expect(css).not.toContain("logo-shimmer");
  });

  it("hover 暂停计时:mouseEnter 后超过 duration 仍在 DOM,mouseLeave 后先 closing 再移除", () => {
    const { container } = render(() => <ToastContainer />);
    addToast({ type: "info", title: "悬停暂停", duration: 3000 });

    const toastEl = container.querySelector(".toast-item") as HTMLElement;
    expect(toastEl).toBeTruthy();

    // hover:组件内部计时器被清除,推进远超 duration 的时间也不应消失
    fireEvent.mouseEnter(toastEl);
    vi.advanceTimersByTime(10000);
    expect(container.querySelector(".toast-item")).not.toBeNull();

    // 离开:重新计时 3000ms,到期先进入 closing 退出态(约 200ms 退出窗口)
    fireEvent.mouseLeave(toastEl);
    vi.advanceTimersByTime(3000);
    const closingEl = container.querySelector(".toast-item");
    expect(closingEl).not.toBeNull();
    expect(closingEl?.classList.contains("closing")).toBe(true);

    // 退出动画窗口结束后才真正从 DOM 移除
    vi.advanceTimersByTime(200);
    expect(container.querySelector(".toast-item")).toBeNull();
  });

  it("不 hover:duration 到期先出现 closing 类(200ms 退出窗口)再移除,非瞬间消失", () => {
    const { container } = render(() => <ToastContainer />);
    addToast({ type: "success", title: "自动关闭", duration: 3000 });

    expect(container.querySelector(".toast-item")).not.toBeNull();

    // duration 未到期:无 closing 类
    vi.advanceTimersByTime(2999);
    const earlyEl = container.querySelector(".toast-item");
    expect(earlyEl?.classList.contains("closing")).toBe(false);

    // duration 到期:先进入 closing 退出态,此时仍在 DOM(退出动画窗口内)
    vi.advanceTimersByTime(1);
    const closingEl = container.querySelector(".toast-item");
    expect(closingEl).not.toBeNull();
    expect(closingEl?.classList.contains("closing")).toBe(true);

    // 200ms 退出窗口结束后才移除
    vi.advanceTimersByTime(200);
    expect(container.querySelector(".toast-item")).toBeNull();
  });
});
