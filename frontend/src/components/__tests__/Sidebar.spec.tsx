import { describe, it, expect, afterEach } from "vitest";
import { render, cleanup } from "@solidjs/testing-library";
import Sidebar from "../Sidebar";

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
