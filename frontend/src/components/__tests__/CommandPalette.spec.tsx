import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { fireEvent, cleanup } from "@solidjs/testing-library";
import { renderPalette, waitForRaf, waitForDebounce } from "./commandPaletteTestUtils";
import {
  addRecentCommand,
  togglePinnedCommand,
  resetCommandHistory,
} from "../../stores/commandHistory";
import { resetAllShortcuts, setShortcut } from "../../stores/shortcuts";
import type { TaskInfo } from "../../types";
import { fuzzySearch } from "../../utils/fuzzySearch";

vi.mock("../../utils/fuzzySearch", async (importOriginal) => {
  const mod = (await importOriginal()) as typeof import("../../utils/fuzzySearch");
  return { ...mod, fuzzySearch: vi.fn(mod.fuzzySearch) };
});

function getOptions(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>('[role="option"]'));
}

function getGroupLabels(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>(".cmd-group-label"));
}

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

describe("CommandPalette", () => {
  beforeEach(() => {
    document.body.focus();
    Element.prototype.scrollIntoView = vi.fn();
    localStorage.clear();
    resetAllShortcuts();
    resetCommandHistory();
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  describe("渲染基础", () => {
    it('open=false 时不渲染 role="dialog"', () => {
      const { container } = renderPalette({ open: false });
      expect(container.querySelector('[role="dialog"]')).toBeNull();
    });

    it('open=true 时渲染输入框、role="listbox"、底部提示', async () => {
      const { container } = renderPalette();
      await waitForRaf();

      expect(container.querySelector('input[type="text"]')).toBeTruthy();
      expect(container.querySelector('[role="listbox"]')).toBeTruthy();
      expect(container.textContent).toContain("Enter");
      expect(container.textContent).toContain("导航");
    });

    it("初始空 query 渲染全部分组及命令", () => {
      const { container } = renderPalette();

      expect(container.textContent).toContain("导航");
      expect(container.textContent).toContain("任务");
      expect(container.textContent).toContain("操作");
      expect(container.textContent).toContain("下载管理");
      expect(container.textContent).toContain("新建下载");
      expect(container.textContent).toContain("全部暂停");
    });

    it("命令项 shortcut badge 从配置读取", async () => {
      const { container } = renderPalette();
      await waitForRaf();

      const shortcuts = Array.from(container.querySelectorAll(".cmd-item-shortcut"));
      expect(shortcuts.length).toBeGreaterThan(0);
      // 默认应显示 Ctrl+B（切换侧边栏）等绑定
      expect(shortcuts.some((s) => s.textContent?.includes("Ctrl"))).toBe(true);
    });

    it("自定义绑定后命令项 badge 同步更新", async () => {
      setShortcut("shortcut.toggleSidebar", ["Ctrl", "Shift", "B"]);
      const { container } = renderPalette();
      await waitForRaf();

      const shortcuts = Array.from(container.querySelectorAll(".cmd-item-shortcut"));
      const toggleShortcut = shortcuts.find((s) =>
        s.parentElement?.textContent?.includes("切换侧边栏"),
      );
      expect(toggleShortcut?.textContent).toContain("Shift");
      expect(toggleShortcut?.textContent).toContain("B");
    });
  });

  describe("搜索过滤与排序", () => {
    it('输入"设置"，结果包含"设置"命令', async () => {
      const { container } = renderPalette();
      const input = container.querySelector(
        'input[type="text"]',
      ) as HTMLInputElement;

      fireEvent.input(input, {
        target: { value: "设置" },
        currentTarget: { value: "设置" },
      });
      await waitForDebounce();

      const options = getOptions(container);
      expect(options.length).toBeGreaterThan(0);
      expect(options.some((o) => o.textContent?.includes("设置"))).toBe(true);
    });

    it('输入"查看"可命中"下载管理"命令', async () => {
      const { container } = renderPalette();
      const input = container.querySelector(
        'input[type="text"]',
      ) as HTMLInputElement;

      fireEvent.input(input, {
        target: { value: "查看" },
        currentTarget: { value: "查看" },
      });
      await waitForDebounce();

      expect(container.textContent).toContain("下载管理");
    });

    it("匹配字符应渲染为 mark 元素", async () => {
      const { container } = renderPalette();
      const input = container.querySelector(
        'input[type="text"]',
      ) as HTMLInputElement;

      fireEvent.input(input, {
        target: { value: "设置" },
        currentTarget: { value: "设置" },
      });
      await waitForDebounce();

      const hasMark =
        container.querySelector("mark") !== null ||
        container.querySelector(".cmd-palette-mark") !== null;
      expect(hasMark).toBe(true);
    });

    it("无匹配 query 显示空状态文本", async () => {
      const { container } = renderPalette();
      const input = container.querySelector(
        'input[type="text"]',
      ) as HTMLInputElement;

      fireEvent.input(input, {
        target: { value: "xyzxyz" },
        currentTarget: { value: "xyzxyz" },
      });
      await waitForDebounce();

      expect(container.textContent).toContain("未找到匹配的命令");
    });

    describe("别名搜索", () => {
      it('输入"bf"命中"资源嗅探"命令', async () => {
        const { container } = renderPalette();
        const input = container.querySelector(
          'input[type="text"]',
        ) as HTMLInputElement;

        fireEvent.input(input, {
          target: { value: "bf" },
          currentTarget: { value: "bf" },
        });
        await waitForDebounce();

        expect(container.textContent).toContain("资源嗅探");
      });

      it('输入"sz"命中"设置"命令', async () => {
        const { container } = renderPalette();
        const input = container.querySelector(
          'input[type="text"]',
        ) as HTMLInputElement;

        fireEvent.input(input, {
          target: { value: "sz" },
          currentTarget: { value: "sz" },
        });
        await waitForDebounce();

        expect(container.textContent).toContain("设置");
      });
    });
  });

  describe("任务搜索", () => {
    it('输入"model"出现"打开任务: model.gguf"且位于 task 分组', async () => {
      const { container } = renderPalette({
        getTasks: () => [
          {
            id: "t1",
            fileName: "model.gguf",
            url: "https://example.com/m",
          },
        ],
      });
      const input = container.querySelector(
        'input[type="text"]',
      ) as HTMLInputElement;

      fireEvent.input(input, {
        target: { value: "model" },
        currentTarget: { value: "model" },
      });
      await waitForDebounce();

      const listbox = container.querySelector(
        '[role="listbox"]',
      ) as HTMLElement;
      expect(listbox.textContent).toContain("任务");
      const taskOption = getOptions(listbox).find((o) =>
        o.textContent?.includes("model.gguf"),
      );
      expect(taskOption).toBeTruthy();
      expect(taskOption!.textContent).toContain("打开任务");
    });
  });

  describe("任务级操作命令上下文", () => {
    const baseTask: TaskInfo = {
      id: "t1",
      url: "https://example.com/file.bin",
      fileName: "file.bin",
      fileSize: 1024,
      downloaded: 0,
      speed: 0,
      status: "downloading",
      progress: 0,
      fragmentsTotal: 1,
      fragmentsDone: 0,
      createdAt: "2024-01-01T00:00:00Z",
      savePath: "",
    };

    it("选中磁力链接任务时显示 task-copy-magnet", async () => {
      const { container } = renderPalette({
        getSelectedTask: () =>
          ({ ...baseTask, url: "magnet:?xt=urn:btih:abc" }) as TaskInfo,
      });
      await waitForRaf();

      expect(container.textContent).toContain("复制磁力链接");
    });

    it("选中带 savePath 的任务时显示 task-open-folder", async () => {
      const { container } = renderPalette({
        getSelectedTask: () =>
          ({ ...baseTask, savePath: "/tmp/download" }) as TaskInfo,
      });
      await waitForRaf();

      expect(container.textContent).toContain("打开保存目录");
    });

    it("普通任务不显示 task-copy-magnet 和 task-open-folder", async () => {
      const { container } = renderPalette({
        getSelectedTask: () => baseTask,
      });
      await waitForRaf();

      expect(container.textContent).not.toContain("复制磁力链接");
      expect(container.textContent).not.toContain("打开保存目录");
    });
  });

  describe("Pinned / Recent", () => {
    it("query 为空时显示 Pinned 分组在最顶部", async () => {
      togglePinnedCommand("nav-settings");
      const { container } = renderPalette();
      await waitForRaf();

      expect(container.textContent).toContain("置顶");
      const groupLabels = getGroupLabels(container);
      expect(groupLabels[0]?.textContent).toBe("置顶");
      expect(container.textContent).toContain("设置");
    });

    it("query 为空时显示 Recent 分组", async () => {
      addRecentCommand("nav-sniffer");
      addRecentCommand("nav-downloads");
      const { container } = renderPalette();
      await waitForRaf();

      expect(container.textContent).toContain("最近使用");
      const recentIdx = getGroupLabels(container).findIndex((g) =>
        g.textContent?.includes("最近使用"),
      );
      expect(recentIdx).toBeGreaterThanOrEqual(0);
    });

    it("Pinned 项不同时出现在 Recent 分组中", async () => {
      togglePinnedCommand("nav-downloads");
      addRecentCommand("nav-downloads");
      addRecentCommand("nav-sniffer");
      const { container } = renderPalette();
      await waitForRaf();

      const groupLabels = getGroupLabels(container);
      const pinnedIdx = groupLabels.findIndex((g) => g.textContent === "置顶");
      const recentIdx = groupLabels.findIndex((g) => g.textContent === "最近使用");
      expect(pinnedIdx).toBeGreaterThanOrEqual(0);
      expect(recentIdx).toBeGreaterThanOrEqual(0);

      const recentLabel = groupLabels[recentIdx];
      let sibling = recentLabel?.nextElementSibling;
      while (sibling && !sibling.classList.contains("cmd-group-label")) {
        expect(sibling.textContent).not.toContain("下载管理");
        sibling = sibling.nextElementSibling;
      }
    });

    it("执行命令后将其加入 Recent", async () => {
      const onViewChange = vi.fn();
      const { container } = renderPalette({ onViewChange });
      await waitForRaf();

      const sniffer = getOptions(container).find((o) =>
        o.textContent?.includes("资源嗅探"),
      );
      expect(sniffer).toBeTruthy();
      fireEvent.click(sniffer!);

      expect(onViewChange).toHaveBeenCalledWith("sniffer");

      const { container: container2 } = renderPalette();
      await waitForRaf();
      expect(container2.textContent).toContain("最近使用");
      expect(container2.textContent).toContain("资源嗅探");
    });

    it("Recent 去重:同一命令只出现一次", async () => {
      addRecentCommand("nav-downloads");
      addRecentCommand("nav-sniffer");
      addRecentCommand("nav-downloads");
      const { container } = renderPalette();
      await waitForRaf();

      const recentLabel = getGroupLabels(container).find(
        (g) => g.textContent === "最近使用",
      );
      let count = 0;
      let sibling = recentLabel?.nextElementSibling;
      while (sibling && !sibling.classList.contains("cmd-group-label")) {
        count++;
        sibling = sibling.nextElementSibling;
      }
      expect(count).toBe(2);
    });
  });

  describe("置顶交互", () => {
    it("Shift+Enter 切换当前选中命令的置顶状态", async () => {
      const { container } = renderPalette();
      await waitForRaf();

      const input = container.querySelector(
        'input[type="text"]',
      ) as HTMLInputElement;
      fireEvent.keyDown(input, { key: "Enter", shiftKey: true });
      await waitForRaf();

      expect(container.textContent).toContain("置顶");
      const pinned = getOptions(container).find(
        (o) => o.previousElementSibling?.textContent === "置顶",
      );
      expect(pinned?.textContent).toContain("下载管理");
    });

    it("点击 pin 按钮切换置顶且不执行命令", async () => {
      const onViewChange = vi.fn();
      const { container } = renderPalette({ onViewChange });
      await waitForRaf();

      const sniffer = getOptions(container).find((o) =>
        o.textContent?.includes("资源嗅探"),
      );
      expect(sniffer).toBeTruthy();

      const pinBtn = sniffer!.querySelector(
        '[aria-label="置顶命令"]',
      ) as HTMLButtonElement;
      expect(pinBtn).toBeTruthy();
      fireEvent.click(pinBtn);
      await waitForRaf();

      expect(onViewChange).not.toHaveBeenCalled();
      expect(container.textContent).toContain("置顶");
      expect(
        getGroupLabels(container).some((g) => g.textContent === "置顶"),
      ).toBe(true);
    });
  });

  describe("键盘导航", () => {
    it("打开后输入框获得焦点", async () => {
      const { container } = renderPalette();
      await waitForRaf();
      await waitForRaf();

      const input = container.querySelector(
        'input[type="text"]',
      ) as HTMLInputElement;
      expect(document.activeElement).toBe(input);
    });

    it("ArrowDown 切换 aria-selected 到下一项", async () => {
      const { container } = renderPalette();
      await waitForRaf();

      const input = container.querySelector(
        'input[type="text"]',
      ) as HTMLInputElement;
      fireEvent.keyDown(input, { key: "ArrowDown" });

      const options = getOptions(container);
      expect(options.length).toBeGreaterThan(1);
      expect(options[0]!.getAttribute("aria-selected")).toBe("false");
      expect(options[1]!.getAttribute("aria-selected")).toBe("true");
    });

    it("ArrowUp 从首项循环到末项", async () => {
      const { container } = renderPalette();
      await waitForRaf();

      const input = container.querySelector(
        'input[type="text"]',
      ) as HTMLInputElement;
      fireEvent.keyDown(input, { key: "ArrowUp" });

      const options = getOptions(container);
      expect(options.length).toBeGreaterThan(0);
      expect(options[0]!.getAttribute("aria-selected")).toBe("false");
      expect(options[options.length - 1]!.getAttribute("aria-selected")).toBe(
        "true",
      );
    });

    it("Enter 执行当前选中命令并关闭面板", async () => {
      const onViewChange = vi.fn();
      const onClose = vi.fn();
      const { container } = renderPalette({ onViewChange, onClose });
      await waitForRaf();

      const input = container.querySelector(
        'input[type="text"]',
      ) as HTMLInputElement;
      fireEvent.keyDown(input, { key: "Enter" });

      expect(onViewChange).toHaveBeenCalledWith("downloads");
      expect(onClose).toHaveBeenCalled();
    });

    it("Escape 触发 onClose", async () => {
      const onClose = vi.fn();
      const { container } = renderPalette({ onClose });
      await waitForRaf();
      await waitForRaf();

      const input = container.querySelector(
        'input[type="text"]',
      ) as HTMLInputElement;
      fireEvent.keyDown(input, { key: "Escape" });
      await new Promise((resolve) => setTimeout(resolve, 200));

      expect(onClose).toHaveBeenCalledTimes(1);
    });
  });

  describe("鼠标交互", () => {
    it("mouseenter 切换 active 项", async () => {
      const { container } = renderPalette();
      await waitForRaf();
      await waitForRaf();

      const options = getOptions(container);
      expect(options.length).toBeGreaterThan(1);
      fireEvent.mouseEnter(options[1]!);
      await waitForRaf();

      expect(options[0]!.getAttribute("aria-selected")).toBe("false");
      expect(options[1]!.getAttribute("aria-selected")).toBe("true");
    });

    it("click 执行命令并关闭面板", async () => {
      const onViewChange = vi.fn();
      const onClose = vi.fn();
      const { container } = renderPalette({ onViewChange, onClose });
      await waitForRaf();

      const target = getOptions(container).find((o) =>
        o.textContent?.includes("资源嗅探"),
      );
      expect(target).toBeTruthy();
      fireEvent.click(target!);

      expect(onViewChange).toHaveBeenCalledWith("sniffer");
      expect(onClose).toHaveBeenCalled();
    });

    it("点击遮罩关闭面板", async () => {
      const onClose = vi.fn();
      const { container } = renderPalette({ onClose });
      await waitForRaf();
      await waitForRaf();

      const dialog = container.querySelector('[role="dialog"]') as HTMLElement;
      fireEvent.click(dialog);
      await new Promise((resolve) => setTimeout(resolve, 200));

      expect(onClose).toHaveBeenCalledTimes(1);
    });
  });

  describe("a11y 语义", () => {
    it('输入框 role="combobox"', async () => {
      const { container } = renderPalette();
      await waitForRaf();

      const input = container.querySelector(
        'input[type="text"]',
      ) as HTMLInputElement;
      expect(input).not.toBeNull();
      expect(input.getAttribute("role")).toBe("combobox");
    });

    it('选项 role="option"', async () => {
      const { container } = renderPalette();
      await waitForRaf();

      expect(getOptions(container).length).toBeGreaterThan(0);
    });

    it("aria-activedescendant 指向当前 option id", async () => {
      const { container } = renderPalette();
      await waitForRaf();

      const input = container.querySelector(
        'input[type="text"]',
      ) as HTMLInputElement;
      const activeDescendant = input.getAttribute("aria-activedescendant");
      expect(activeDescendant).toBeTruthy();

      const activeOption = container.querySelector<HTMLElement>(
        `[id="${activeDescendant}"]`,
      );
      expect(activeOption).toBeTruthy();
      expect(activeOption!.getAttribute("aria-selected")).toBe("true");
    });

    it("存在 aria-live 区域", async () => {
      const { container } = renderPalette();
      await waitForRaf();

      expect(container.querySelector("[aria-live]")).toBeTruthy();
    });
  });

  describe("性能优化", () => {
    beforeEach(() => {
      vi.mocked(fuzzySearch).mockClear();
    });

    it("搜索输入防抖:连续按键只触发一次过滤", async () => {
      const { container } = renderPalette({
        debounceMs: 100,
        getTasks: () => [
          { id: "t1", fileName: "model.gguf", url: "https://example.com/m" },
        ],
      });
      await waitForRaf();

      const input = container.querySelector(
        'input[type="text"]',
      ) as HTMLInputElement;
      // 确保测量前 mock 计数归零,避免并行/异步初始化导致基线不一致
      vi.mocked(fuzzySearch).mockClear();
      const callsBefore = vi.mocked(fuzzySearch).mock.calls.length;

      fireEvent.input(input, {
        target: { value: "a" },
        currentTarget: { value: "a" },
      });
      await waitForDebounce();
      fireEvent.input(input, {
        target: { value: "ab" },
        currentTarget: { value: "ab" },
      });
      await waitForDebounce();
      fireEvent.input(input, {
        target: { value: "abc" },
        currentTarget: { value: "abc" },
      });
      await waitForDebounce();

      // 防抖结束前不应执行新的过滤
      expect(vi.mocked(fuzzySearch).mock.calls.length).toBe(callsBefore);

      // 等待最后一次防抖到期
      await new Promise<void>((resolve) => setTimeout(resolve, 120));

      // 最终只按最终 query 多执行一轮命令+任务过滤
      expect(vi.mocked(fuzzySearch).mock.calls.length).toBe(callsBefore + 2);
      const lastCall = vi.mocked(fuzzySearch).mock.calls.at(-1);
      expect(lastCall?.[1]).toBe("abc");
    });

    it("activeIndex 变化不应重新执行过滤", async () => {
      const { container } = renderPalette({ debounceMs: 0 });
      await waitForDebounce();

      const input = container.querySelector(
        'input[type="text"]',
      ) as HTMLInputElement;

      fireEvent.input(input, {
        target: { value: "设置" },
        currentTarget: { value: "设置" },
      });
      await waitForDebounce();

      const callsAfterFilter = vi.mocked(fuzzySearch).mock.calls.length;

      fireEvent.keyDown(input, { key: "ArrowDown" });
      fireEvent.keyDown(input, { key: "ArrowDown" });
      fireEvent.keyDown(input, { key: "ArrowUp" });
      await waitForRaf();

      expect(vi.mocked(fuzzySearch).mock.calls.length).toBe(callsAfterFilter);
    });
  });

  describe("移动端窄屏适配", () => {
    beforeEach(() => {
      mockMatchMedia(true);
    });

    afterEach(() => {
      vi.unstubAllGlobals();
    });

    it("小屏下命令面板添加 cmd-panel--narrow 类", async () => {
      const { container } = renderPalette();
      await waitForRaf();

      const panel = container.querySelector(".cmd-panel");
      expect(panel).toBeTruthy();
      expect(panel!.classList.contains("cmd-panel--narrow")).toBe(true);
    });
  });
});
