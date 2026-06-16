import { describe, it, expect, afterEach, beforeEach, vi } from "vitest";
import type { JSX } from "solid-js";
import { render, cleanup, fireEvent } from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../i18n";
import TaskList from "../TaskList";

const renderWithI18n = (ui: () => JSX.Element) =>
  render(() => <I18nProvider i18n={i18n}>{ui()}</I18nProvider>);

describe("TaskList 空状态与交互", () => {
  beforeEach(() => {
    vi.stubGlobal(
      "ResizeObserver",
      vi.fn().mockImplementation(() => ({
        observe: vi.fn(),
        disconnect: vi.fn(),
        unobserve: vi.fn(),
      })),
    );
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it("无任务时应显示空状态与新建按钮", () => {
    const onNewTask = vi.fn();
    const { container } = renderWithI18n(() => (
      <TaskList
        tasks={[]}
        selectedTaskId={null}
        onTaskClick={() => {}}
        isMultiSelectMode={false}
        selectedTaskIds={new Set()}
        density="comfortable"
        onNewTask={onNewTask}
      />
    ));

    expect(container.textContent).toContain("暂无下载任务");
    const btn = container.querySelector("button");
    expect(btn).toBeDefined();

    fireEvent.click(btn!);
    expect(onNewTask).toHaveBeenCalledTimes(1);
  });

  it("可排序列头应具备焦点环样式", () => {
    const { container } = renderWithI18n(() => (
      <TaskList
        tasks={[]}
        selectedTaskId={null}
        onTaskClick={() => {}}
        isMultiSelectMode={false}
        selectedTaskIds={new Set()}
        density="comfortable"
      />
    ));

    const headers = container.querySelectorAll('[role="columnheader"]');
    expect(headers.length).toBeGreaterThan(0);

    headers.forEach((h) => {
      if (h.hasAttribute("tabindex")) {
        const className = h.className;
        expect(className).toContain("focus:outline-none");
        expect(className).toContain("focus-visible:focus-ring");
      }
    });
  });
});
