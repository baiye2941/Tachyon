import { describe, it, expect, afterEach, vi } from "vitest";
import { render, cleanup, screen, fireEvent } from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../../i18n";
import EmptyState from "../EmptyState";
import type { JSX } from "solid-js";

const renderWithI18n = (ui: () => JSX.Element) =>
  render(() => <I18nProvider i18n={i18n}>{ui()}</I18nProvider>);

describe("EmptyState", () => {
  afterEach(() => {
    cleanup();
  });

  it("渲染标题", () => {
    renderWithI18n(() => (
      <EmptyState icon={<span data-testid="icon" />} title="暂无内容" />
    ));

    expect(screen.getByText("暂无内容")).toBeDefined();
    expect(screen.getByTestId("icon")).toBeDefined();
  });

  it("渲染描述与操作按钮", () => {
    const onClick = vi.fn();
    renderWithI18n(() => (
      <EmptyState
        icon={<span data-testid="icon" />}
        title="空状态标题"
        description="空状态描述"
        action={{ label: "去创建", onClick, icon: <span>+</span> }}
      />
    ));

    expect(screen.getByText("空状态描述")).toBeDefined();
    const btn = screen.getByRole("button", { name: /去创建/ });
    expect(btn).toBeDefined();

    fireEvent.click(btn);
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("支持自定义 aria-label", () => {
    renderWithI18n(() => (
      <EmptyState
        icon={<span data-testid="icon" />}
        title="空状态标题"
        action={{ label: "去创建", onClick: () => {}, ariaLabel: "创建新任务" }}
      />
    ));

    expect(screen.getByRole("button", { name: "创建新任务" })).toBeDefined();
  });

  it("未传 action 时不渲染按钮", () => {
    const { container } = renderWithI18n(() => (
      <EmptyState icon={<span data-testid="icon" />} title="无操作" />
    ));

    expect(container.querySelector("button")).toBeNull();
  });

  it("渲染 children 插槽", () => {
    const { container } = renderWithI18n(() => (
      <EmptyState icon={<span data-testid="icon" />} title="有提示">
        <div data-testid="hints">提示内容</div>
      </EmptyState>
    ));

    expect(container.querySelector('[data-testid="hints"]')).toBeDefined();
    expect(screen.getByText("提示内容")).toBeDefined();
  });

  it("compact 模式应用紧凑样式", () => {
    const { container } = renderWithI18n(() => (
      <EmptyState
        compact
        icon={<span data-testid="icon" />}
        title="紧凑空状态"
      />
    ));

    expect(
      container.querySelector(".empty-state.empty-state--compact"),
    ).toBeDefined();
  });

  it("brand 模式应用品牌图标样式", () => {
    const { container } = renderWithI18n(() => (
      <EmptyState
        brand
        icon={<span data-testid="icon" />}
        title="品牌空状态"
      />
    ));

    expect(
      container.querySelector(".empty-state-icon.empty-state-icon--brand"),
    ).toBeDefined();
  });
});
