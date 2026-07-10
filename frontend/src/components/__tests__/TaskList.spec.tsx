import { describe, it, expect, afterEach, beforeEach, vi } from "vitest";
import type { JSX } from "solid-js";
import { render, cleanup, fireEvent, screen } from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../i18n";
import TaskList from "../TaskList";
import type { TaskInfo } from "../../types";
import { $taskColumns } from "../../stores/taskColumnsConfig";
import { toggleSort, clearSort } from "../../stores/taskSort";

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

const noopTaskClick = (
  _taskId: string,
  _index: number,
  _shiftKey: boolean,
  _orderedTaskIds: string[],
) => {};

const noopHandlers = () => ({
  onTaskActivate: () => {},
  onSelectRange: () => {},
  onSelectAll: () => {},
  onDeleteSelected: () => {},
});

const makeHandlers = () => ({
  onTaskActivate: vi.fn(),
  onSelectRange: vi.fn(),
  onSelectAll: vi.fn(),
  onDeleteSelected: vi.fn(),
});

describe("TaskList 空状态与交互", () => {
  beforeEach(() => {
    $taskColumns.resetColumns();
    clearSort();
    localStorage.clear();

    // vitest 4: mockImplementation 返回普通对象时 `new` 失败(not a constructor)。
    // 用 class 形式确保 `new ResizeObserver(...)` 返回带 observe/disconnect 的实例。
    vi.stubGlobal(
      "ResizeObserver",
      vi.fn().mockImplementation(function (this: ResizeObserver, callback: ResizeObserverCallback) {
        return {
          observe: vi.fn(() => {
            callback(
              [
                {
                  contentRect: { height: 1000 },
                  target: document.createElement("div"),
                  borderBoxSize: [],
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

  it("无任务时应显示空状态与新建按钮", () => {
    const onNewTask = vi.fn();
    const { container } = renderWithI18n(() => (
      <TaskList
        tasks={[]}
        selectedTaskId={null}
        onTaskClick={noopTaskClick}
        isMultiSelectMode={false}
        selectedTaskIds={new Set()}
        density="comfortable"
        onNewTask={onNewTask}
        keyboardHandlers={noopHandlers()}
      />
    ));

    expect(container.textContent).toContain("暂无下载任务");
    const btn = screen.getByRole("button", { name: /新建下载任务/ });
    expect(btn).toBeDefined();

    fireEvent.click(btn);
    expect(onNewTask).toHaveBeenCalledTimes(1);
  });

  it("默认渲染 name/progress/speed/status 四列表头", () => {
    const { container } = renderWithI18n(() => (
      <TaskList
        tasks={[]}
        selectedTaskId={null}
        onTaskClick={noopTaskClick}
        isMultiSelectMode={false}
        selectedTaskIds={new Set()}
        density="comfortable"
        keyboardHandlers={noopHandlers()}
      />
    ));

    const headers = container.querySelectorAll('[role="columnheader"]');
    expect(headers.length).toBe(4);
    expect(headers[0]?.textContent).toContain("文件名");
    expect(headers[1]?.textContent).toContain("进度");
    expect(headers[2]?.textContent).toContain("速度");
    expect(headers[3]?.textContent).toContain("状态");
  });

  it("可排序列头应具备焦点环样式", () => {
    const { container } = renderWithI18n(() => (
      <TaskList
        tasks={[]}
        selectedTaskId={null}
        onTaskClick={noopTaskClick}
        isMultiSelectMode={false}
        selectedTaskIds={new Set()}
        density="comfortable"
        keyboardHandlers={noopHandlers()}
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

  it("列设置面板可切换列显示", () => {
    const { container } = renderWithI18n(() => (
      <TaskList
        tasks={[]}
        selectedTaskId={null}
        onTaskClick={noopTaskClick}
        isMultiSelectMode={false}
        selectedTaskIds={new Set()}
        density="comfortable"
        keyboardHandlers={noopHandlers()}
      />
    ));

    const settingsBtn = container.querySelector(".task-list-settings-button");
    expect(settingsBtn).not.toBeNull();
    fireEvent.click(settingsBtn!);

    const sizeCheckbox = screen.getByRole("checkbox", { name: /大小/ });
    expect(sizeCheckbox).not.toBeNull();
    fireEvent.click(sizeCheckbox!);

    const headers = container.querySelectorAll('[role="columnheader"]');
    expect(headers.length).toBe(5);
    expect(Array.from(headers).some((h) => h.textContent?.includes("大小"))).toBe(true);
  });

  it("再次点击齿轮按钮可关闭列设置面板", () => {
    const { container } = renderWithI18n(() => (
      <TaskList
        tasks={[]}
        selectedTaskId={null}
        onTaskClick={noopTaskClick}
        isMultiSelectMode={false}
        selectedTaskIds={new Set()}
        density="comfortable"
        keyboardHandlers={noopHandlers()}
      />
    ));

    const settingsBtn = container.querySelector(".task-list-settings-button");
    expect(settingsBtn).not.toBeNull();

    fireEvent.click(settingsBtn!);
    expect(container.querySelector(".column-settings-panel")).not.toBeNull();

    fireEvent.click(settingsBtn!);
    expect(container.querySelector(".column-settings-panel")).toBeNull();
  });

  it("列宽通过 store 同步到表头 inline style", () => {
    $taskColumns.setWidth("progress", 200);

    const { container } = renderWithI18n(() => (
      <TaskList
        tasks={[]}
        selectedTaskId={null}
        onTaskClick={noopTaskClick}
        isMultiSelectMode={false}
        selectedTaskIds={new Set()}
        density="comfortable"
        keyboardHandlers={noopHandlers()}
      />
    ));

    const headers = container.querySelectorAll('[role="columnheader"]');
    const progressHeader = Array.from(headers).find((h) =>
      h.textContent?.includes("进度"),
    );
    expect(progressHeader).not.toBeNull();
    expect(progressHeader?.getAttribute("style")).toContain("width: 200px");
  });

  it("隐藏当前排序列时自动清除排序", () => {
    toggleSort("speed");

    const { container } = renderWithI18n(() => (
      <TaskList
        tasks={[]}
        selectedTaskId={null}
        onTaskClick={noopTaskClick}
        isMultiSelectMode={false}
        selectedTaskIds={new Set()}
        density="comfortable"
        keyboardHandlers={noopHandlers()}
      />
    ));

    // 打开设置，隐藏 speed 列
    const settingsBtn = container.querySelector(".task-list-settings-button");
    fireEvent.click(settingsBtn!);
    const speedCheckbox = screen.getByRole("checkbox", { name: /速度/ });
    fireEvent.click(speedCheckbox!);

    const headers = container.querySelectorAll('[role="columnheader"]');
    expect(Array.from(headers).some((h) => h.textContent?.includes("速度"))).toBe(false);
  });

  describe("键盘导航", () => {
    const getListbox = (container: HTMLElement) => {
      const listbox = container.querySelector('[role="listbox"]');
      expect(listbox).not.toBeNull();
      return listbox as HTMLElement;
    };

    it("ArrowDown 应激活第一项(单选模式)并更新 aria-activedescendant", () => {
      const handlers = makeHandlers();
      const { container } = renderWithI18n(() => (
        <TaskList
          tasks={[makeTask("t1"), makeTask("t2"), makeTask("t3")]}
          selectedTaskId={null}
          onTaskClick={noopTaskClick}
          isMultiSelectMode={false}
          selectedTaskIds={new Set()}
          density="comfortable"
          keyboardHandlers={handlers}
        />
      ));

      const listbox = getListbox(container);
      listbox.focus();
      fireEvent.keyDown(listbox, { key: "ArrowDown" });

      expect(handlers.onTaskActivate).toHaveBeenCalledWith("t1", 0);
      expect(listbox.getAttribute("aria-activedescendant")).toBe(
        "task-item-t1",
      );
    });

    it("ArrowUp 应从末尾开始激活(单选模式)", () => {
      const handlers = makeHandlers();
      const { container } = renderWithI18n(() => (
        <TaskList
          tasks={[makeTask("t1"), makeTask("t2"), makeTask("t3")]}
          selectedTaskId={null}
          onTaskClick={noopTaskClick}
          isMultiSelectMode={false}
          selectedTaskIds={new Set()}
          density="comfortable"
          keyboardHandlers={handlers}
        />
      ));

      const listbox = getListbox(container);
      listbox.focus();
      fireEvent.keyDown(listbox, { key: "ArrowUp" });

      expect(handlers.onTaskActivate).toHaveBeenCalledWith("t3", 2);
    });

    it("Shift + ArrowDown 应调用 onSelectRange 扩展选择", () => {
      const handlers = makeHandlers();
      const { container } = renderWithI18n(() => (
        <TaskList
          tasks={[makeTask("t1"), makeTask("t2"), makeTask("t3")]}
          selectedTaskId={null}
          onTaskClick={noopTaskClick}
          isMultiSelectMode={true}
          selectedTaskIds={new Set()}
          density="comfortable"
          keyboardHandlers={handlers}
        />
      ));

      const listbox = getListbox(container);
      listbox.focus();
      fireEvent.keyDown(listbox, { key: "ArrowDown" });
      fireEvent.keyDown(listbox, { key: "ArrowDown", shiftKey: true });

      expect(handlers.onSelectRange).toHaveBeenCalledWith(0, 1, [
        "t1",
        "t2",
        "t3",
      ]);
    });

    it("连续 Shift + ArrowDown 应保持同一锚点扩展", () => {
      const handlers = makeHandlers();
      const { container } = renderWithI18n(() => (
        <TaskList
          tasks={[makeTask("t1"), makeTask("t2"), makeTask("t3"), makeTask("t4")]}
          selectedTaskId={null}
          onTaskClick={noopTaskClick}
          isMultiSelectMode={true}
          selectedTaskIds={new Set()}
          density="comfortable"
          keyboardHandlers={handlers}
        />
      ));

      const listbox = getListbox(container);
      listbox.focus();
      fireEvent.keyDown(listbox, { key: "ArrowDown" });
      fireEvent.keyDown(listbox, { key: "ArrowDown", shiftKey: true });
      fireEvent.keyDown(listbox, { key: "ArrowDown", shiftKey: true });

      expect(handlers.onSelectRange).toHaveBeenLastCalledWith(0, 2, [
        "t1",
        "t2",
        "t3",
        "t4",
      ]);
      expect(handlers.onSelectRange).toHaveBeenCalledTimes(2);
    });

    it("Space 应触发当前活动项的 onTaskClick", () => {
      const onTaskClick = vi.fn();
      const handlers = makeHandlers();
      const { container } = renderWithI18n(() => (
        <TaskList
          tasks={[makeTask("t1"), makeTask("t2")]}
          selectedTaskId={null}
          onTaskClick={onTaskClick}
          isMultiSelectMode={true}
          selectedTaskIds={new Set()}
          density="comfortable"
          keyboardHandlers={handlers}
        />
      ));

      const listbox = getListbox(container);
      listbox.focus();
      fireEvent.keyDown(listbox, { key: "ArrowDown" });
      fireEvent.keyDown(listbox, { key: " " });

      expect(onTaskClick).toHaveBeenCalledWith("t1", 0, false, ["t1", "t2"]);
    });

    it("Ctrl + A 应调用 onSelectAll", () => {
      const handlers = makeHandlers();
      const { container } = renderWithI18n(() => (
        <TaskList
          tasks={[makeTask("t1"), makeTask("t2")]}
          selectedTaskId={null}
          onTaskClick={noopTaskClick}
          isMultiSelectMode={false}
          selectedTaskIds={new Set()}
          density="comfortable"
          keyboardHandlers={handlers}
        />
      ));

      const listbox = getListbox(container);
      listbox.focus();
      fireEvent.keyDown(listbox, { key: "a", ctrlKey: true });

      expect(handlers.onSelectAll).toHaveBeenCalledTimes(1);
    });

    it("Delete 应调用 onDeleteSelected", () => {
      const handlers = makeHandlers();
      const { container } = renderWithI18n(() => (
        <TaskList
          tasks={[makeTask("t1"), makeTask("t2")]}
          selectedTaskId={null}
          onTaskClick={noopTaskClick}
          isMultiSelectMode={false}
          selectedTaskIds={new Set()}
          density="comfortable"
          keyboardHandlers={handlers}
        />
      ));

      const listbox = getListbox(container);
      listbox.focus();
      fireEvent.keyDown(listbox, { key: "Delete" });

      expect(handlers.onDeleteSelected).toHaveBeenCalledTimes(1);
    });
  });

  describe("分组视图", () => {
    const getListbox = (container: HTMLElement) => {
      const listbox = container.querySelector('[role="listbox"]');
      expect(listbox).not.toBeNull();
      return listbox as HTMLElement;
    };

    it("默认平铺视图不渲染 group header", () => {
      const { container } = renderWithI18n(() => (
        <TaskList
          tasks={[
            makeTask("t1", { status: "downloading" }),
            makeTask("t2", { status: "completed" }),
          ]}
          selectedTaskId={null}
          onTaskClick={noopTaskClick}
          isMultiSelectMode={false}
          selectedTaskIds={new Set()}
          density="comfortable"
          keyboardHandlers={noopHandlers()}
        />
      ));

      expect(container.querySelectorAll(".task-group-header").length).toBe(0);
    });

    it("切换分组视图后渲染非空 group header", () => {
      const { container } = renderWithI18n(() => (
        <TaskList
          tasks={[
            makeTask("t1", { status: "downloading" }),
            makeTask("t2", { status: "completed" }),
            makeTask("t3", { status: "paused" }),
          ]}
          selectedTaskId={null}
          groupBy="status"
          onTaskClick={noopTaskClick}
          isMultiSelectMode={false}
          selectedTaskIds={new Set()}
          density="comfortable"
          keyboardHandlers={noopHandlers()}
        />
      ));

      const headers = container.querySelectorAll(".task-group-header");
      expect(headers.length).toBe(3);
      expect(Array.from(headers).some((h) => h.textContent?.includes("活跃中"))).toBe(true);
      expect(Array.from(headers).some((h) => h.textContent?.includes("已完成"))).toBe(true);
      expect(Array.from(headers).some((h) => h.textContent?.includes("已暂停"))).toBe(true);
    });

    it("空组不显示 header", () => {
      const { container } = renderWithI18n(() => (
        <TaskList
          tasks={[makeTask("t1", { status: "downloading" })]}
          selectedTaskId={null}
          groupBy="status"
          onTaskClick={noopTaskClick}
          isMultiSelectMode={false}
          selectedTaskIds={new Set()}
          density="comfortable"
          keyboardHandlers={noopHandlers()}
        />
      ));

      const headers = container.querySelectorAll(".task-group-header");
      expect(headers.length).toBe(1);
      expect(headers[0]?.textContent).toContain("活跃中");
    });

    it("点击 header 折叠/展开", () => {
      const { container } = renderWithI18n(() => (
        <TaskList
          tasks={[
            makeTask("t1", { status: "downloading" }),
            makeTask("t2", { status: "downloading" }),
          ]}
          selectedTaskId={null}
          groupBy="status"
          onTaskClick={noopTaskClick}
          isMultiSelectMode={false}
          selectedTaskIds={new Set()}
          density="comfortable"
          keyboardHandlers={noopHandlers()}
        />
      ));

      let header = container.querySelector(".task-group-header");
      expect(header).not.toBeNull();
      expect(header?.getAttribute("aria-expanded")).toBe("true");

      fireEvent.click(header!);
      header = container.querySelector(".task-group-header");
      expect(header?.getAttribute("aria-expanded")).toBe("false");

      fireEvent.click(header!);
      header = container.querySelector(".task-group-header");
      expect(header?.getAttribute("aria-expanded")).toBe("true");
    });

    it("Enter 在 header 上切换折叠", () => {
      const { container } = renderWithI18n(() => (
        <TaskList
          tasks={[makeTask("t1", { status: "downloading" })]}
          selectedTaskId={null}
          groupBy="status"
          onTaskClick={noopTaskClick}
          isMultiSelectMode={false}
          selectedTaskIds={new Set()}
          density="comfortable"
          keyboardHandlers={noopHandlers()}
        />
      ));

      const listbox = getListbox(container);
      listbox.focus();
      fireEvent.keyDown(listbox, { key: "ArrowDown" });

      let header = container.querySelector(".task-group-header");
      expect(header).not.toBeNull();
      expect(header?.getAttribute("aria-expanded")).toBe("true");

      fireEvent.keyDown(listbox, { key: "Enter" });
      header = container.querySelector(".task-group-header");
      expect(header?.getAttribute("aria-expanded")).toBe("false");
    });

    it("ArrowDown 跨 header 导航", () => {
      const handlers = makeHandlers();
      const { container } = renderWithI18n(() => (
        <TaskList
          tasks={[
            makeTask("t1", { status: "downloading" }),
            makeTask("t2", { status: "completed" }),
          ]}
          selectedTaskId={null}
          groupBy="status"
          onTaskClick={noopTaskClick}
          isMultiSelectMode={false}
          selectedTaskIds={new Set()}
          density="comfortable"
          keyboardHandlers={handlers}
        />
      ));

      const listbox = getListbox(container);
      listbox.focus();
      fireEvent.keyDown(listbox, { key: "ArrowDown" });
      expect(listbox.getAttribute("aria-activedescendant")).toBe(
        "task-group-header-active",
      );

      fireEvent.keyDown(listbox, { key: "ArrowDown" });
      expect(listbox.getAttribute("aria-activedescendant")).toBe(
        "task-item-t1",
      );

      fireEvent.keyDown(listbox, { key: "ArrowDown" });
      expect(listbox.getAttribute("aria-activedescendant")).toBe(
        "task-group-header-completed",
      );

      fireEvent.keyDown(listbox, { key: "ArrowDown" });
      expect(listbox.getAttribute("aria-activedescendant")).toBe(
        "task-item-t2",
      );
    });

    it("Shift + ArrowDown 不选中 header", () => {
      const handlers = makeHandlers();
      const { container } = renderWithI18n(() => (
        <TaskList
          tasks={[
            makeTask("t1", { status: "downloading" }),
            makeTask("t2", { status: "completed" }),
          ]}
          selectedTaskId={null}
          groupBy="status"
          onTaskClick={noopTaskClick}
          isMultiSelectMode={true}
          selectedTaskIds={new Set()}
          density="comfortable"
          keyboardHandlers={handlers}
        />
      ));

      const listbox = getListbox(container);
      listbox.focus();
      // 先定位到 t1（跳过 active header）；多选模式下不触发 onTaskActivate
      fireEvent.keyDown(listbox, { key: "ArrowDown" });
      fireEvent.keyDown(listbox, { key: "ArrowDown" });
      expect(handlers.onTaskActivate).not.toHaveBeenCalled();

      // 再 Shift + ArrowDown，应跳过 completed header 跳到 t2 并扩展选择
      fireEvent.keyDown(listbox, { key: "ArrowDown", shiftKey: true });
      expect(handlers.onSelectRange).toHaveBeenCalledWith(0, 1, ["t1", "t2"]);
    });
  });
});
