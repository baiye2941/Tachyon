import { describe, it, expect, beforeEach, afterEach } from "vitest";
import type { JSX } from "solid-js";
import { render, cleanup, fireEvent, screen } from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../i18n";
import StatusBar from "../StatusBar";

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
