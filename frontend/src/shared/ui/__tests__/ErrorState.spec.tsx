import { describe, it, expect, afterEach, vi } from "vitest";
import { render, cleanup, screen, fireEvent } from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../../i18n";
import ErrorState from "../ErrorState";
import type { JSX } from "solid-js";

const renderWithI18n = (ui: () => JSX.Element) =>
  render(() => <I18nProvider i18n={i18n}>{ui()}</I18nProvider>);

describe("ErrorState", () => {
  afterEach(() => {
    cleanup();
  });

  it("默认渲染通用错误标题与重试按钮", () => {
    const onRetry = vi.fn();
    renderWithI18n(() => <ErrorState onRetry={onRetry} />);

    expect(screen.getByText("应用发生错误")).toBeDefined();
    const btn = screen.getByRole("button", { name: "重试" });
    expect(btn).toBeDefined();

    fireEvent.click(btn);
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("渲染自定义标题、消息与详情", () => {
    renderWithI18n(() => (
      <ErrorState
        title="加载失败"
        message="无法连接到服务器"
        detail="ECONNREFUSED"
      />
    ));

    expect(screen.getByText("加载失败")).toBeDefined();
    expect(screen.getByText("无法连接到服务器")).toBeDefined();
    expect(screen.getByText("ECONNREFUSED")).toBeDefined();
  });

  it("自定义重试标签", () => {
    const onRetry = vi.fn();
    renderWithI18n(() => (
      <ErrorState onRetry={onRetry} retryLabel="重新加载" />
    ));

    expect(screen.getByRole("button", { name: "重新加载" })).toBeDefined();
  });

  it("未传 onRetry 时不渲染重试按钮", () => {
    const { container } = renderWithI18n(() => (
      <ErrorState title="仅展示" message="无需重试" />
    ));

    expect(container.querySelector("button")).toBeNull();
  });

  it("compact 模式应用紧凑样式", () => {
    const { container } = renderWithI18n(() => (
      <ErrorState compact title="紧凑错误" />
    ));

    expect(
      container.querySelector(".error-state.error-state--compact"),
    ).toBeDefined();
  });

  it("具备 alert 角色", () => {
    const { container } = renderWithI18n(() => <ErrorState title="错误" />);

    expect(container.querySelector('[role="alert"]')).toBeDefined();
  });
});
