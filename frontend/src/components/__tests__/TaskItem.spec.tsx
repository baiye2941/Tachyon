import { describe, it, expect, afterEach, beforeEach, vi } from "vitest";
import type { JSX } from "solid-js";
import { render, cleanup, fireEvent, screen } from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../i18n";
import type { TaskInfo } from "../../types";
import TaskItem from "../TaskItem";
import { $taskColumns } from "../../stores/taskColumnsConfig";

const renderWithI18n = (ui: () => JSX.Element) =>
  render(() => <I18nProvider i18n={i18n}>{ui()}</I18nProvider>);

const task: TaskInfo = {
  id: "task-1",
  url: "https://example.com/annual-report-2025.pdf",
  fileName: "annual-report-2025.pdf",
  fileSize: 24.6 * 1024 * 1024,
  downloaded: 24.6 * 1024 * 1024,
  speed: 0,
  status: "completed",
  progress: 1,
  fragmentsTotal: 12,
  fragmentsDone: 12,
  createdAt: "2026-06-25T08:00:00.000Z",
  savePath: "D:\\Downloads\\annual-report-2025.pdf",
};

describe("TaskItem", () => {
  beforeEach(() => {
    $taskColumns.resetColumns();
    localStorage.clear();
  });

  afterEach(() => {
    cleanup();
  });

  it("Enter 和 Space 应触发任务点击", () => {
    const onClick = vi.fn();
    renderWithI18n(() => (
      <TaskItem
        task={task}
        index={0}
        isSelected={false}
        isMultiSelected={false}
        isMultiSelectMode={false}
        onClick={onClick}
        density="comfortable"
      />
    ));

    const row = screen.getByRole("button", { name: /annual-report-2025\.pdf/ });
    fireEvent.keyDown(row, { key: "Enter" });
    fireEvent.keyDown(row, { key: " " });

    expect(onClick).toHaveBeenCalledTimes(2);
    expect(onClick).toHaveBeenLastCalledWith(false);
  });

  it("多选模式应显示 checkbox 并保留 aria-checked", () => {
    renderWithI18n(() => (
      <TaskItem
        task={task}
        index={0}
        isSelected={false}
        isMultiSelected
        isMultiSelectMode
        onClick={() => {}}
        density="comfortable"
      />
    ));

    const checkbox = screen.getByRole("checkbox", {
      name: /annual-report-2025\.pdf/,
    });
    expect(checkbox.getAttribute("aria-checked")).toBe("true");
  });

  it("搜索高亮应处理特殊字符", () => {
    renderWithI18n(() => (
      <TaskItem
        task={{ ...task, fileName: "file(1)+backup.pdf" }}
        index={0}
        isSelected={false}
        isMultiSelected={false}
        isMultiSelectMode={false}
        onClick={() => {}}
        density="comfortable"
        searchQuery="file(1)+"
      />
    ));

    const mark = document.querySelector("mark.search-highlight");
    expect(mark?.textContent).toBe("file(1)+");
  });

  it("Shift + 点击应将 shiftKey 透传给 onClick", () => {
    const onClick = vi.fn();
    renderWithI18n(() => (
      <TaskItem
        task={task}
        index={2}
        isSelected={false}
        isMultiSelected={false}
        isMultiSelectMode={false}
        onClick={onClick}
        density="comfortable"
      />
    ));

    const row = screen.getByRole("button", { name: /annual-report-2025\.pdf/ });
    fireEvent.click(row, { shiftKey: true });

    expect(onClick).toHaveBeenCalledWith(true);
  });

  it("应渲染扩展名胶囊、进度和状态", () => {
    const { container } = renderWithI18n(() => (
      <TaskItem
        task={{ ...task, status: "failed", progress: 0.12 }}
        index={0}
        isSelected={false}
        isMultiSelected={false}
        isMultiSelectMode={false}
        onClick={() => {}}
        density="comfortable"
      />
    ));

    expect(container.textContent).toContain("pdf");
    expect(container.textContent).toContain("12.0%");
    expect(container.textContent).toContain("出错");
    expect(container.querySelector('[role="progressbar"]')).toBeTruthy();
    expect(container.querySelector(".status-badge--failed")).toBeTruthy();
  });

  it("按列配置渲染新增列（大小、分片、线程、创建时间）", () => {
    $taskColumns.toggleVisibility("size");
    $taskColumns.toggleVisibility("fragments");
    $taskColumns.toggleVisibility("threads");
    $taskColumns.toggleVisibility("createdAt");

    const { container } = renderWithI18n(() => (
      <TaskItem
        task={{ ...task, activeConcurrency: 8 }}
        index={0}
        isSelected={false}
        isMultiSelected={false}
        isMultiSelectMode={false}
        onClick={() => {}}
        density="comfortable"
      />
    ));

    const text = container.textContent;
    expect(text).toContain("24.6 MB"); // size
    expect(text).toContain("12/12"); // fragments
    expect(text).toContain("8"); // threads
    expect(text).toContain("2026-06-25"); // createdAt
  });

  it("下载中状态的速度列显示活跃色", () => {
    const { container } = renderWithI18n(() => (
      <TaskItem
        task={{ ...task, status: "downloading", speed: 1024 * 1024 }}
        index={0}
        isSelected={false}
        isMultiSelected={false}
        isMultiSelectMode={false}
        onClick={() => {}}
        density="comfortable"
      />
    ));

    const activeSpeedCell = container.querySelector(
      ".task-list-cell--active-speed",
    );
    expect(activeSpeedCell).not.toBeNull();
  });
});
