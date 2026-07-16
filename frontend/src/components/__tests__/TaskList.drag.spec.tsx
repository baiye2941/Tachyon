import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import type { JSX } from "solid-js";
import { render, cleanup, fireEvent, waitFor } from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../i18n";
import TaskList from "../TaskList";
import type { TaskInfo } from "../../types";
import { $taskColumns } from "../../stores/taskColumnsConfig";
import { clearSort } from "../../stores/taskSort";

const mockMoveTask = vi.fn();
const mockReorderTasks = vi.fn();

vi.mock("../../api/invoke", () => ({
  api: {
    getTaskList: vi.fn().mockResolvedValue([]),
    moveTask: (...args: unknown[]) => mockMoveTask(...args),
    reorderTasks: (...args: unknown[]) => mockReorderTasks(...args),
  },
}));

const renderWithI18n = (ui: () => JSX.Element) =>
  render(() => <I18nProvider i18n={i18n}>{ui()}</I18nProvider>);

const makeTask = (id: string, overrides: Partial<TaskInfo> = {}): TaskInfo => ({
  id,
  url: `https://example.com/${id}.bin`,
  fileName: `${id}.bin`,
  fileSize: 1048576,
  downloaded: 0,
  speed: 0,
  status: "downloading",
  progress: 0.5,
  fragmentsTotal: 4,
  fragmentsDone: 2,
  createdAt: "2026-05-30T00:00:00Z",
  savePath: "/downloads",
  ...overrides,
});

const noopHandlers = () => ({
  onTaskActivate: () => {},
  onSelectRange: () => {},
  onSelectAll: () => {},
  onDeleteSelected: () => {},
});

describe("TaskList 拖拽排序", () => {
  beforeEach(() => {
    $taskColumns.resetColumns();
    clearSort();
    mockMoveTask.mockReset();
    mockReorderTasks.mockReset();
    mockMoveTask.mockResolvedValue(undefined);

    vi.stubGlobal(
      "ResizeObserver",
      vi.fn().mockImplementation(function (this: ResizeObserver, callback: ResizeObserverCallback) {
        return {
          observe: vi.fn((target: Element) => {
            callback(
              [
                {
                  target,
                  contentRect: { width: 800, height: 600 },
                  borderBoxSize: [{ inlineSize: 800, blockSize: 600 }],
                  contentBoxSize: [],
                  devicePixelContentBoxSize: [],
                } as unknown as ResizeObserverEntry,
              ],
              this,
            );
          }),
          disconnect: vi.fn(),
          unobserve: vi.fn(),
        };
      }),
    );
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it("平铺未排序时任务行显示拖拽手柄", async () => {
    const { container } = renderWithI18n(() => (
      <TaskList
        tasks={[makeTask("t1"), makeTask("t2"), makeTask("t3"), makeTask("t4"), makeTask("t5")]}
        selectedTaskId={null}
        onTaskClick={() => {}}
        isMultiSelectMode={false}
        selectedTaskIds={new Set()}
        density="comfortable"
        keyboardHandlers={noopHandlers()}
      />
    ));

    await waitFor(() => {
      expect(container.querySelectorAll(".task-drag-handle").length).toBeGreaterThan(0);
    });
  });

  it("分组视图下不显示拖拽手柄", async () => {
    const { container } = renderWithI18n(() => (
      <TaskList
        tasks={[makeTask("t1"), makeTask("t2"), makeTask("t3"), makeTask("t4"), makeTask("t5")]}
        selectedTaskId={null}
        groupBy="status"
        onTaskClick={() => {}}
        isMultiSelectMode={false}
        selectedTaskIds={new Set()}
        density="comfortable"
        keyboardHandlers={noopHandlers()}
      />
    ));

    await waitFor(() => {
      expect(container.querySelectorAll('[role="option"]').length).toBeGreaterThan(0);
    });

    expect(container.querySelector(".task-drag-handle")).toBeNull();
  });

  it("排序激活时不显示拖拽手柄", async () => {
    // 触发 speed 列排序
    const { container } = renderWithI18n(() => (
      <TaskList
        tasks={[makeTask("t1"), makeTask("t2"), makeTask("t3"), makeTask("t4"), makeTask("t5")]}
        selectedTaskId={null}
        onTaskClick={() => {}}
        isMultiSelectMode={false}
        selectedTaskIds={new Set()}
        density="comfortable"
        keyboardHandlers={noopHandlers()}
      />
    ));

    await waitFor(() => {
      expect(container.querySelectorAll('[role="option"]').length).toBeGreaterThan(0);
    });

    const speedHeader = Array.from(container.querySelectorAll('.task-list-col--sortable')).find(
      (el) => el.textContent?.includes("速度"),
    );
    if (speedHeader) {
      fireEvent.click(speedHeader);
    }

    await waitFor(() => {
      expect(container.querySelector(".task-drag-handle")).toBeNull();
    });
  });

  it("拖放到另一任务上调用 moveTask", async () => {
    const tasks = [makeTask("t1"), makeTask("t2"), makeTask("t3"), makeTask("t4"), makeTask("t5")];
    const { container } = renderWithI18n(() => (
      <TaskList
        tasks={tasks}
        selectedTaskId={null}
        onTaskClick={() => {}}
        isMultiSelectMode={false}
        selectedTaskIds={new Set()}
        density="comfortable"
        keyboardHandlers={noopHandlers()}
      />
    ));

    await waitFor(() => {
      expect(container.querySelectorAll(".task-drag-handle").length).toBeGreaterThan(0);
    });

    const handles = container.querySelectorAll(".task-drag-handle");
    const rows = container.querySelectorAll('[role="option"]');
    expect(rows.length).toBeGreaterThanOrEqual(2);

    const sourceHandle = handles[0]!;
    const targetRow = rows[1]!;

    fireEvent.dragStart(sourceHandle, {
      dataTransfer: {
        setData: vi.fn(),
        effectAllowed: "none",
      },
    });

    fireEvent.dragOver(targetRow);

    fireEvent.drop(targetRow, {
      dataTransfer: {
        getData: vi.fn().mockReturnValue("t1"),
      },
    });

    await waitFor(() => {
      expect(mockMoveTask).toHaveBeenCalled();
    });
  });
});
