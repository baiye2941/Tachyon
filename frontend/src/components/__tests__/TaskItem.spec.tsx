import { describe, it, expect, afterEach, vi } from "vitest";
import type { JSX } from "solid-js";
import { render, cleanup, fireEvent, screen } from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../i18n";
import type { TaskInfo } from "../../types";
import TaskItem from "../TaskItem";

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
  afterEach(() => {
    cleanup();
  });

  it("Enter 和 Space 应触发任务点击", () => {
    const onClick = vi.fn();
    renderWithI18n(() => (
      <TaskItem
        task={task}
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
  });

  it("多选模式应显示 checkbox 并保留 aria-checked", () => {
    renderWithI18n(() => (
      <TaskItem
        task={task}
        isSelected={false}
        isMultiSelected
        isMultiSelectMode
        onClick={() => {}}
        density="comfortable"
      />
    ));

    const checkbox = screen.getByRole("checkbox", { name: /annual-report-2025\.pdf/ });
    expect(checkbox.getAttribute("aria-checked")).toBe("true");
  });

  it("搜索高亮应处理特殊字符", () => {
    renderWithI18n(() => (
      <TaskItem
        task={{ ...task, fileName: "file(1)+backup.pdf" }}
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

  it("应渲染扩展名胶囊、进度和状态", () => {
    const { container } = renderWithI18n(() => (
      <TaskItem
        task={{ ...task, status: "failed", progress: 0.12 }}
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
    expect(container.querySelector(".linear-progress-fill--failed")).toBeTruthy();
  });
});
