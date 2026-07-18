import { describe, it, expect, beforeEach, afterEach } from "vitest";
import type { JSX } from "solid-js";
import { render, cleanup, fireEvent, screen } from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../i18n";
import StatusBar from "../StatusBar";
import { pushSpeed, clearHistory } from "../../stores/speedHistory";

const THEME_KEY = "tachyon-theme";

const renderWithI18n = (ui: () => JSX.Element) =>
  render(() => <I18nProvider i18n={i18n}>{ui()}</I18nProvider>);

const defaultProps = {
  isIdle: true,
  totalSpeed: 0,
  activeCount: 0,
  pausedCount: 0,
  totalCount: 0,
};

function renderStatusBar(props: Partial<typeof defaultProps> = {}) {
  return renderWithI18n(() => <StatusBar {...defaultProps} {...props} />);
}

describe("StatusBar 主题切换按钮", () => {
  beforeEach(() => {
    localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
  });

  afterEach(() => {
    cleanup();
  });

  it("渲染主题切换按钮(可由 aria-label 定位)", () => {
    renderStatusBar();
    const btn = screen.getByLabelText("切换明暗主题");
    expect(btn).toBeDefined();
    expect(btn.tagName).toBe("BUTTON");
  });

  it("点击主题切换按钮触发 toggleTheme 并切换 data-theme", () => {
    renderStatusBar();

    const btn = screen.getByLabelText("切换明暗主题");
    fireEvent.click(btn);

    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
    expect(localStorage.getItem(THEME_KEY)).toBe("light");

    // 再点切回 dark
    fireEvent.click(btn);
    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
    expect(localStorage.getItem(THEME_KEY)).toBe("dark");
  });

  it("暗色时 title 为明亮主题(切到亮色用),亮色时为暗黑主题", () => {
    renderStatusBar();

    const btn = screen.getByLabelText("切换明暗主题");

    // 初始 dark -> SunIcon:title="明亮主题"
    expect(btn.getAttribute("title")).toBe("明亮主题");

    fireEvent.click(btn);
    // 切到 light -> MoonIcon:title="暗黑主题"
    expect(btn.getAttribute("title")).toBe("暗黑主题");
  });

  it("不再渲染限速/反馈 disabled 占位按钮", () => {
    const { container } = renderStatusBar();

    // 限速按钮(aria-label/title "限速设置")与反馈按钮("反馈")应不存在
    expect(screen.queryByLabelText("限速设置")).toBeNull();
    expect(screen.queryByLabelText("反馈")).toBeNull();

    // 仅剩主题切换 + 语言切换两个按钮(原先限速/反馈两个占位已移除)
    const buttons = container.querySelectorAll("button");
    expect(buttons.length).toBe(2);
  });

  it("主题切换按钮为 ghost icon-sm 变体", () => {
    renderStatusBar();
    const btn = screen.getByLabelText("切换明暗主题");
    expect(btn.className).toContain("icon-btn-sm");
    expect(btn.className).toContain("btn-icon-sm");
  });
});

describe("StatusBar sparkline 峰值标记", () => {
  beforeEach(() => {
    clearHistory();
  });

  afterEach(() => {
    cleanup();
    clearHistory();
  });

  it("速度 sparkline 标记历史峰值点", () => {
    pushSpeed(100);
    pushSpeed(500);
    pushSpeed(200);

    const { container } = renderStatusBar({
      isIdle: false,
      totalSpeed: 1024,
      activeCount: 1,
      pausedCount: 0,
      totalCount: 1,
    });

    const peak = container.querySelector(".sparkline-peak");
    expect(peak).toBeTruthy();
    // 峰值 500 在索引 1:cx = 1/(3-1) × 80(默认宽) = 40
    expect(peak?.getAttribute("cx")).toBe("40");
  });

  it("数据不足 2 个点时不渲染峰值标记", () => {
    pushSpeed(100);

    const { container } = renderStatusBar({
      isIdle: false,
      totalSpeed: 100,
      activeCount: 1,
      pausedCount: 0,
      totalCount: 1,
    });

    expect(container.querySelector(".sparkline-peak")).toBeNull();
  });
});
