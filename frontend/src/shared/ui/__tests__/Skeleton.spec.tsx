import { describe, it, expect, afterEach } from "vitest";
import { render, screen, cleanup } from "@solidjs/testing-library";
import Skeleton from "../Skeleton";

describe("Skeleton", () => {
  afterEach(() => {
    cleanup();
  });

  it("渲染 status 角色与 aria-busy", () => {
    render(() => <Skeleton variant="card" />);
    const el = screen.getByRole("status");
    expect(el).toBeDefined();
    expect(el.getAttribute("aria-busy")).toBe("true");
  });

  it("默认使用 common.loading 作为 aria-label", () => {
    render(() => <Skeleton variant="panel" />);
    const el = screen.getByRole("status");
    expect(el.getAttribute("aria-label")).toBe("加载中...");
  });

  it("支持自定义 label", () => {
    render(() => <Skeleton variant="dialog" label="正在加载设置..." />);
    const el = screen.getByRole("status");
    expect(el.getAttribute("aria-label")).toBe("正在加载设置...");
  });

  it("panel 变体渲染 header 与 body 行", () => {
    const { container } = render(() => <Skeleton variant="panel" />);
    expect(container.querySelector(".skeleton--panel")).toBeDefined();
    expect(container.querySelectorAll(".skeleton__row").length).toBeGreaterThan(
      0,
    );
  });

  it("dialog 变体渲染对话框卡片结构", () => {
    const { container } = render(() => <Skeleton variant="dialog" />);
    expect(container.querySelector(".skeleton--dialog")).toBeDefined();
    expect(container.querySelector(".skeleton__dialog-card")).toBeDefined();
    expect(
      container.querySelectorAll(".skeleton__dialog-body .skeleton__row"),
    ).toBeDefined();
  });

  it("list 变体渲染搜索栏与列表行", () => {
    const { container } = render(() => <Skeleton variant="list" />);
    expect(container.querySelector(".skeleton--list")).toBeDefined();
    expect(container.querySelector(".skeleton__search")).toBeDefined();
    expect(container.querySelector(".skeleton__list")).toBeDefined();
  });
});
