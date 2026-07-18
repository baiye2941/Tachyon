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

  it("按列配置渲染新增列（大小、分片、并发分片、创建时间）", () => {
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
    expect(text).toContain("8"); // concurrency
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

  it("下载中任务行渲染微型光迹元素", () => {
    const { container } = renderWithI18n(() => (
      <TaskItem
        task={{
          ...task,
          status: "downloading",
          progress: 0.5,
          downloaded: 512,
          speed: 100,
        }}
        index={0}
        isSelected={false}
        isMultiSelected={false}
        isMultiSelectMode={false}
        onClick={() => {}}
        density="comfortable"
      />
    ));

    const trail = container.querySelector(".task-item-light-trail");
    expect(trail).toBeTruthy();
  });

  it("非下载中任务行不渲染微型光迹", () => {
    const { container } = renderWithI18n(() => (
      <TaskItem
        task={{ ...task, status: "completed", progress: 1 }}
        index={0}
        isSelected={false}
        isMultiSelected={false}
        isMultiSelectMode={false}
        onClick={() => {}}
        density="comfortable"
      />
    ));

    expect(container.querySelector(".task-item-light-trail")).toBeNull();
  });

  it("作为 listbox option 时，选中行应设置 aria-selected=true", () => {
    renderWithI18n(() => (
      <TaskItem
        task={task}
        index={0}
        isSelected={true}
        isMultiSelected={false}
        isMultiSelectMode={false}
        onClick={() => {}}
        density="comfortable"
        role="option"
      />
    ));

    const row = screen.getByRole("option");
    expect(row.getAttribute("aria-selected")).toBe("true");
  });

  it("作为 listbox option 时，多选选中行也应设置 aria-selected=true", () => {
    renderWithI18n(() => (
      <TaskItem
        task={task}
        index={0}
        isSelected={false}
        isMultiSelected={true}
        isMultiSelectMode={true}
        onClick={() => {}}
        density="comfortable"
        role="option"
      />
    ));

    const row = screen.getByRole("option");
    expect(row.getAttribute("aria-selected")).toBe("true");
  });

  it("role=button 时不应设置 aria-selected", () => {
    renderWithI18n(() => (
      <TaskItem
        task={task}
        index={0}
        isSelected={true}
        isMultiSelected={false}
        isMultiSelectMode={false}
        onClick={() => {}}
        density="comfortable"
      />
    ));

    const row = screen.getByRole("button");
    expect(row.getAttribute("aria-selected")).toBeNull();
  });

  it("审计 FT-04:热进度覆盖 cold task 的 progress 显示", async () => {
    const { setTasks, updateProgress } = await import("../../stores/downloads");
    const cold: TaskInfo = {
      ...task,
      id: "hot-1",
      status: "downloading",
      progress: 0.1,
      speed: 100,
      downloaded: 100,
      fragmentsDone: 0,
    };
    setTasks([cold]);
    updateProgress({
      "hot-1": {
        id: "hot-1",
        progress: 0.77,
        downloaded: 770,
        speed: 2500,
        status: "downloading",
        fragmentsDone: 3,
        fragmentsTotal: 10,
        activeConcurrency: 2,
      },
    });

    renderWithI18n(() => (
      <TaskItem
        task={cold}
        index={0}
        isSelected={false}
        isMultiSelected={false}
        isMultiSelectMode={false}
        onClick={() => {}}
        density="comfortable"
      />
    ));

    // aria-label 使用 liveProgress → 77.0%
    const row = screen.getByRole("button", { name: /77\.0/ });
    expect(row).toBeTruthy();
  });
});
