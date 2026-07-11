import { describe, it, expect, afterEach, beforeEach, vi } from "vitest";
import type { JSX } from "solid-js";
import { createRoot } from "solid-js";
import { render, cleanup, fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../i18n";
import TaskList from "../TaskList";
import type { TaskInfo } from "../../types";
import { $taskColumns } from "../../stores/taskColumnsConfig";
import { toggleSort, clearSort } from "../../stores/taskSort";
import { $ui } from "../../stores/ui";
import {
  $onboarding,
  resetOnboarding,
  completeOnboarding,
} from "../../stores/onboarding";

function mockMatchMedia(matches: boolean) {
  const listeners: ((e: MediaQueryListEvent) => void)[] = [];
  const mql = {
    matches,
    media: "",
    onchange: null,
    addEventListener: (
      _type: string,
      listener: (e: MediaQueryListEvent) => void,
    ) => listeners.push(listener),
    removeEventListener: (
      _type: string,
      listener: (e: MediaQueryListEvent) => void,
    ) => {
      const i = listeners.indexOf(listener);
      if (i >= 0) listeners.splice(i, 1);
    },
    dispatchEvent: () => true,
    addListener: () => {},
    removeListener: () => {},
  };
  vi.stubGlobal("matchMedia", () => mql);
}

const renderWithI18n = (ui: () => JSX.Element) =>
  render(() => <I18nProvider i18n={i18n}>{ui()}</I18nProvider>);

function readSignal<T>(fn: () => T): T {
  return createRoot((dispose) => {
    try {
      return fn();
    } finally {
      dispose();
    }
  });
}

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
    resetOnboarding();

    // vitest 4: mockImplementation 返回普通对象时 `new` 失败(not a constructor)。
    // 用 class 形式确保 `new ResizeObserver(...)` 返回带 observe/disconnect 的实例。
    // 提供 borderBoxSize,使 @tanstack/solid-virtual 能正确读取视口高度。
    vi.stubGlobal(
      "ResizeObserver",
      vi.fn().mockImplementation(function (this: ResizeObserver, callback: ResizeObserverCallback) {
        return {
          observe: vi.fn((target: Element) => {
            callback(
              [
                {
                  target,
                  contentRect: { width: 800, height: 1000 },
                  borderBoxSize: [
                    { inlineSize: 800, blockSize: 1000 },
                  ],
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
    expect(readSignal(() => $ui.newTaskModalOpen())).toBe(true);
  });

  it("空列表应展示 Onboarding 引导提示", () => {
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

    expect(container.textContent).toContain("拖拽文件到窗口");
    expect(container.textContent).toContain("从 HuggingFace 浏览");
    expect(container.textContent).toContain("识别剪贴板 URL");
  });

  it("首次使用时空列表新建按钮高亮", () => {
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

    const btn = container.querySelector("button[data-highlight='true']");
    expect(btn).not.toBeNull();
  });

  it("非首次使用时空列表新建按钮不高亮", () => {
    completeOnboarding();
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

    const btn = container.querySelector("button[data-highlight='true']");
    expect(btn).toBeNull();
  });

  it("点击空列表新建按钮后标记引导完成", () => {
    renderWithI18n(() => (
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

    const btn = screen.getByRole("button", { name: /新建下载任务/ });
    fireEvent.click(btn);

    expect(readSignal(() => $onboarding.isCompleted())).toBe(true);
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

  it("表头应具有 scope=col", () => {
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
      expect(h.getAttribute("scope")).toBe("col");
    });
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

  describe("移动端窄屏适配", () => {
    beforeEach(() => {
      mockMatchMedia(true);
    });

    afterEach(() => {
      vi.unstubAllGlobals();
    });

    it("小屏下根容器添加 task-list--narrow 类", () => {
      const { container } = renderWithI18n(() => (
        <TaskList
          tasks={[makeTask("t1")]}
          selectedTaskId={null}
          onTaskClick={noopTaskClick}
          isMultiSelectMode={false}
          selectedTaskIds={new Set()}
          density="comfortable"
          keyboardHandlers={noopHandlers()}
        />
      ));

      const root = container.firstElementChild;
      expect(root).toBeTruthy();
      expect(root!.classList.contains("task-list--narrow")).toBe(true);
    });
  });
});

describe("TaskList 虚拟滚动", () => {
  const ITEM_HEIGHT = 72;
  const VIEWPORT_HEIGHT = 300;

  const makeTasks = (count: number): TaskInfo[] =>
    Array.from({ length: count }, (_, i) => ({
      id: `task-${i}`,
      url: `https://example.com/file-${i}.bin`,
      fileName: `file-${i}.bin`,
      fileSize: 1024 * 1024,
      downloaded: 0,
      speed: 0,
      status: "downloading",
      progress: 0.5,
      fragmentsTotal: 10,
      fragmentsDone: 5,
      createdAt: new Date(Date.now() - i * 1000).toISOString(),
      savePath: `/downloads/file-${i}.bin`,
    }));

  class TestResizeObserver implements ResizeObserver {
    callback: ResizeObserverCallback;
    targets: Element[] = [];
    static instances: TestResizeObserver[] = [];

    constructor(cb: ResizeObserverCallback) {
      this.callback = cb;
      TestResizeObserver.instances.push(this);
    }

    observe(target: Element) {
      this.targets.push(target);
    }

    unobserve(target: Element) {
      this.targets = this.targets.filter((t) => t !== target);
    }

    disconnect() {
      this.targets = [];
    }

    trigger(rect: { width: number; height: number }) {
      const entry = {
        target: this.targets[0],
        contentRect: {
          x: 0,
          y: 0,
          width: rect.width,
          height: rect.height,
          top: 0,
          right: rect.width,
          bottom: rect.height,
          left: 0,
          toJSON: () => "",
        },
        borderBoxSize: [
          {
            inlineSize: rect.width,
            blockSize: rect.height,
          },
        ],
        contentBoxSize: [],
        devicePixelContentBoxSize: [],
      } as unknown as ResizeObserverEntry;
      this.callback([entry], this);
    }
  }

  function setViewport(container: HTMLElement, height = VIEWPORT_HEIGHT) {
    const listbox = container.querySelector('[role="listbox"]') as HTMLElement;
    expect(listbox).not.toBeNull();

    Object.defineProperty(listbox, "offsetWidth", {
      value: 800,
      configurable: true,
    });
    Object.defineProperty(listbox, "offsetHeight", {
      value: height,
      configurable: true,
    });
    Object.defineProperty(listbox, "clientWidth", {
      value: 800,
      configurable: true,
    });
    Object.defineProperty(listbox, "clientHeight", {
      value: height,
      configurable: true,
    });
    Object.defineProperty(listbox, "scrollWidth", {
      value: 800,
      configurable: true,
    });
    Object.defineProperty(listbox, "scrollTo", {
      value: vi.fn(),
      configurable: true,
    });

    for (const ro of TestResizeObserver.instances) {
      if (ro.targets.includes(listbox)) {
        ro.trigger({ width: 800, height });
      }
    }

    return listbox;
  }

  function scrollTo(listbox: HTMLElement, top: number) {
    Object.defineProperty(listbox, "scrollTop", {
      value: top,
      configurable: true,
    });
    Object.defineProperty(listbox, "scrollHeight", {
      value: Math.max(top + VIEWPORT_HEIGHT, (listbox.scrollHeight as number) || 0),
      configurable: true,
    });
    fireEvent.scroll(listbox);
  }

  beforeEach(() => {
    localStorage.clear();
    $taskColumns.resetColumns();
    resetOnboarding();
    TestResizeObserver.instances = [];
    vi.stubGlobal("ResizeObserver", TestResizeObserver);
    vi.stubGlobal(
      "matchMedia",
      vi.fn().mockImplementation((query: string) => ({
        matches: false,
        media: query,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        dispatchEvent: vi.fn(),
      })),
    );
    vi.spyOn(Element.prototype, "getBoundingClientRect").mockReturnValue({
      x: 0,
      y: 0,
      width: 800,
      height: VIEWPORT_HEIGHT,
      top: 0,
      right: 800,
      bottom: VIEWPORT_HEIGHT,
      left: 0,
      toJSON: () => "",
    });
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("100 个任务仅渲染视口内 + overscan 的项", async () => {
    const { container } = renderWithI18n(() => (
      <TaskList
        tasks={makeTasks(100)}
        selectedTaskId={null}
        onTaskClick={noopTaskClick}
        isMultiSelectMode={false}
        selectedTaskIds={new Set()}
        density="comfortable"
        keyboardHandlers={noopHandlers()}
      />
    ));

    const listbox = setViewport(container);

    await waitFor(() => {
      const rows = listbox.querySelectorAll('[role="option"]');
      expect(rows.length).toBeGreaterThan(0);
      // 可见约 5 项 + 上下 overscan,不应超过 15 项
      expect(rows.length).toBeLessThan(15);
    });

    expect(listbox.textContent).toContain("file-0.bin");
    expect(listbox.textContent).not.toContain("file-99.bin");
  });

  it("滚动后渲染新的可见项", async () => {
    const { container } = renderWithI18n(() => (
      <TaskList
        tasks={makeTasks(100)}
        selectedTaskId={null}
        onTaskClick={noopTaskClick}
        isMultiSelectMode={false}
        selectedTaskIds={new Set()}
        density="comfortable"
        keyboardHandlers={noopHandlers()}
      />
    ));

    const listbox = setViewport(container);

    await waitFor(() => {
      expect(listbox.querySelectorAll('[role="option"]').length).toBeGreaterThan(0);
    });

    scrollTo(listbox, 50 * ITEM_HEIGHT);

    await waitFor(() => {
      const text = listbox.textContent ?? "";
      expect(text).toContain("file-50.bin");
      expect(text).not.toContain("file-0.bin");
      expect(text).not.toContain("file-99.bin");
    });
  });

  it("选择状态在滚动后保留", async () => {
    const { container } = renderWithI18n(() => (
      <TaskList
        tasks={makeTasks(100)}
        selectedTaskId={null}
        onTaskClick={noopTaskClick}
        isMultiSelectMode={true}
        selectedTaskIds={new Set(["task-5"])}
        density="comfortable"
        keyboardHandlers={noopHandlers()}
      />
    ));

    const listbox = setViewport(container);

    await waitFor(() => {
      expect(listbox.querySelectorAll('[role="option"]').length).toBeGreaterThan(0);
    });

    const selectedBefore = listbox.querySelector('[aria-selected="true"]');
    expect(selectedBefore?.textContent).toContain("file-5.bin");

    scrollTo(listbox, 80 * ITEM_HEIGHT);
    await waitFor(() => {
      expect(listbox.textContent).toContain("file-80.bin");
    });

    scrollTo(listbox, 0);
    await waitFor(() => {
      expect(listbox.textContent).toContain("file-5.bin");
    });

    const selectedAfter = listbox.querySelector('[aria-selected="true"]');
    expect(selectedAfter?.textContent).toContain("file-5.bin");
  });
});
