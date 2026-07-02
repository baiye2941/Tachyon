import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { fireEvent, cleanup } from "@solidjs/testing-library";
import { renderPalette, waitForRaf } from "./commandPaletteTestUtils";

describe("CommandPalette", () => {
  beforeEach(() => {
    document.body.focus();
    Element.prototype.scrollIntoView = vi.fn();
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
      await waitForRaf();

      const options = container.querySelectorAll('[role="option"]');
      expect(options.length).toBeGreaterThan(0);
      expect(
        Array.from(options).some((o) => o.textContent?.includes("设置")),
      ).toBe(true);
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
      await waitForRaf();

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
      await waitForRaf();

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
      await waitForRaf();

      expect(container.textContent).toContain("未找到匹配的命令");
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
      await waitForRaf();

      const listbox = container.querySelector(
        '[role="listbox"]',
      ) as HTMLElement;
      expect(listbox.textContent).toContain("任务");
      const options = listbox.querySelectorAll('[role="option"]');
      const taskOption = Array.from(options).find((o) =>
        o.textContent?.includes("model.gguf"),
      );
      expect(taskOption).toBeTruthy();
      expect(taskOption!.textContent).toContain("打开任务");
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

      const options =
        container.querySelectorAll<HTMLElement>('[role="option"]');
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

      const options =
        container.querySelectorAll<HTMLElement>('[role="option"]');
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

      const options =
        container.querySelectorAll<HTMLElement>('[role="option"]');
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

      const options =
        container.querySelectorAll<HTMLElement>('[role="option"]');
      const target = Array.from(options).find((o) =>
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

      const options = container.querySelectorAll('[role="option"]');
      expect(options.length).toBeGreaterThan(0);
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
});
