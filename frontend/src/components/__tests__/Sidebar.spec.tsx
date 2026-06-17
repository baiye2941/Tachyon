import { describe, it, expect, afterEach, beforeEach } from "vitest";
import { render, cleanup } from "@solidjs/testing-library";
import Sidebar from "../Sidebar";
import { $ui } from "../../stores/ui";

const SIDEBAR_KEY = "tachyon-sidebar-state";

describe("Sidebar 可访问性", () => {
  afterEach(() => {
    cleanup();
  });

  it("NavItem 应渲染为 button 且支持 aria-current", () => {
    const { container } = render(() => <Sidebar />);

    const buttons = container.querySelectorAll('button[aria-current="page"]');
    expect(buttons.length).toBeGreaterThan(0);

    const firstNav = container.querySelector("button[aria-label]");
    expect(firstNav).toBeDefined();
    expect(firstNav!.getAttribute("title")).toBeTruthy();
  });

  it("NavItem 应具备 focus-visible 样式类", () => {
    const { container } = render(() => <Sidebar />);

    const navButtons = container.querySelectorAll(".sidebar-nav-item");
    expect(navButtons.length).toBeGreaterThan(0);

    navButtons.forEach((btn) => {
      const className = btn.className;
      expect(className).toContain("focus:outline-none");
      expect(className).toContain("focus-visible:focus-ring");
    });
  });
});

describe("Sidebar 双轨伸缩(Iteration 12/13)", () => {
  beforeEach(() => {
    // 重置 store 到已知状态:非 pinned、collapsed
    localStorage.removeItem(SIDEBAR_KEY);
    // 通过 store action 重置(而非 localStorage,因 store 已初始化)
    if ($ui.sidebarPinned()) $ui.toggleSidebarPin();
    $ui.setSidebarCollapsed(true);
  });

  afterEach(() => {
    cleanup();
    localStorage.removeItem(SIDEBAR_KEY);
    if ($ui.sidebarPinned()) $ui.toggleSidebarPin();
    $ui.setSidebarCollapsed(true);
  });

  it("默认 collapsed 态占位宽度为 RAIL_WIDTH(56px)", () => {
    const { container } = render(() => <Sidebar />);
    const placeholder = container.querySelector(
      ".relative.flex-shrink-0.h-full.overflow-hidden",
    ) as HTMLElement | null;
    expect(placeholder).not.toBeNull();
    expect(placeholder!.style.width).toBe("56px");
  });

  it("pin 后占位宽度从 RAIL 扩展到面板宽度", () => {
    const { container } = render(() => <Sidebar />);
    const placeholder = () =>
      container.querySelector(
        ".relative.flex-shrink-0.h-full.overflow-hidden",
      ) as HTMLElement;
    expect(placeholder().style.width).toBe("56px");

    $ui.toggleSidebarPin();

    const w = parseInt(placeholder().style.width, 10);
    expect(w).toBeGreaterThanOrEqual(200);
  });

  it("collapsed 态轨道应渲染图标按钮(非空),提供 collapsed 交互", () => {
    const { container } = render(() => <Sidebar />);
    const navItems = container.querySelectorAll(".sidebar-nav-item");
    expect(navItems.length).toBeGreaterThan(5);
  });

  it("展开后(setSidebarCollapsed=false)占位宽度变为面板宽度", () => {
    const { container } = render(() => <Sidebar />);
    const placeholder = () =>
      container.querySelector(
        ".relative.flex-shrink-0.h-full.overflow-hidden",
      ) as HTMLElement;
    expect(placeholder().style.width).toBe("56px");

    $ui.setSidebarCollapsed(false);

    const w = parseInt(placeholder().style.width, 10);
    expect(w).toBeGreaterThanOrEqual(200);
  });

  it("展开面板用 transform 定位(collapsed 时藏到轨道后)", () => {
    // 确保 collapsed
    if (!$ui.sidebarCollapsed()) $ui.setSidebarCollapsed(true);
    const { container } = render(() => <Sidebar />);
    // 展开面板:z-index:2 的绝对定位层
    const panels = container.querySelectorAll("[style*='translateX']");
    expect(panels.length).toBeGreaterThan(0);
    // collapsed 态面板应有负 translateX(藏到轨道后)
    const collapsedPanel = Array.from(panels).find((p) =>
      (p as HTMLElement).style.transform.includes("translateX(-"),
    ) as HTMLElement | undefined;
    expect(collapsedPanel).not.toBeUndefined();
  });
});
