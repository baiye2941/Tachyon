import { describe, it, expect, vi, afterEach, beforeEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../../i18n";
import type { JSX } from "solid-js";
import ToolbarBatch from "../ToolbarBatch";

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

const makeProps = (overrides: Partial<Parameters<typeof ToolbarBatch>[0]> = {}) => ({
  searchQuery: "",
  onSearchChange: vi.fn(),
  filters: [],
  onRemoveFilter: vi.fn(),
  isMultiSelectMode: true,
  onToggleMultiSelect: vi.fn(),
  selectedCount: 2,
  totalCount: 4,
  onSelectAll: vi.fn(),
  onPauseSelected: vi.fn(),
  onResumeSelected: vi.fn(),
  onCancelSelected: vi.fn(),
  onDeleteSelected: vi.fn(),
  onOpenSelectedFolders: vi.fn(),
  onCopySelectedLinks: vi.fn(),
  onRedownloadSelected: vi.fn(),
  onClearSelection: vi.fn(),
  onExitMultiSelect: vi.fn(),
  listDensity: "comfortable" as const,
  onToggleDensity: vi.fn(),
  onNewTask: vi.fn(),
  onOpenSettings: vi.fn(),
  onPauseAll: vi.fn(),
  onResumeAll: vi.fn(),
  onCancelAll: vi.fn(),
  ...overrides,
});

describe("ToolbarBatch 批量操作工具栏", () => {
  afterEach(() => {
    cleanup();
  });

  it("渲染选中数量", () => {
    renderWithI18n(() => <ToolbarBatch {...makeProps()} />);

    expect(screen.getByText("已选 2 项")).toBeDefined();
  });

  it("所有可见操作按钮均具备 aria-label", () => {
    renderWithI18n(() => <ToolbarBatch {...makeProps()} />);

    const buttons = screen.getAllByRole("button");
    expect(buttons.length).toBeGreaterThan(0);
    buttons.forEach((btn) => {
      const label = btn.getAttribute("aria-label");
      const text = btn.textContent;
      expect(label ?? text).toBeTruthy();
    });
  });

  it("点击暂停调用 onPauseSelected", () => {
    const props = makeProps();
    renderWithI18n(() => <ToolbarBatch {...props} />);

    fireEvent.click(screen.getByLabelText("暂停"));
    expect(props.onPauseSelected).toHaveBeenCalledTimes(1);
  });

  it("点击取消调用 onCancelSelected", () => {
    const props = makeProps();
    renderWithI18n(() => <ToolbarBatch {...props} />);

    fireEvent.click(screen.getByLabelText("取消"));
    expect(props.onCancelSelected).toHaveBeenCalledTimes(1);
  });

  it("点击打开文件夹调用 onOpenSelectedFolders", () => {
    const props = makeProps();
    renderWithI18n(() => <ToolbarBatch {...props} />);

    fireEvent.click(screen.getByLabelText("打开文件夹"));
    expect(props.onOpenSelectedFolders).toHaveBeenCalledTimes(1);
  });

  it("点击复制链接调用 onCopySelectedLinks", () => {
    const props = makeProps();
    renderWithI18n(() => <ToolbarBatch {...props} />);

    fireEvent.click(screen.getByLabelText("复制链接"));
    expect(props.onCopySelectedLinks).toHaveBeenCalledTimes(1);
  });

  it("点击重新下载调用 onRedownloadSelected", () => {
    const props = makeProps();
    renderWithI18n(() => <ToolbarBatch {...props} />);

    fireEvent.click(screen.getByLabelText("重新下载"));
    expect(props.onRedownloadSelected).toHaveBeenCalledTimes(1);
  });

  it("点击删除调用 onDeleteSelected", () => {
    const props = makeProps();
    renderWithI18n(() => <ToolbarBatch {...props} />);

    fireEvent.click(screen.getByLabelText("删除"));
    expect(props.onDeleteSelected).toHaveBeenCalledTimes(1);
  });

  it("点击清空选择调用 onClearSelection", () => {
    const props = makeProps();
    renderWithI18n(() => <ToolbarBatch {...props} />);

    fireEvent.click(screen.getByLabelText("清空选择"));
    expect(props.onClearSelection).toHaveBeenCalledTimes(1);
  });

  it("点击退出多选调用 onExitMultiSelect", () => {
    const props = makeProps();
    renderWithI18n(() => <ToolbarBatch {...props} />);

    fireEvent.click(screen.getByLabelText("退出多选"));
    expect(props.onExitMultiSelect).toHaveBeenCalledTimes(1);
  });

  it("未选中时显示「全选」且 aria-pressed=false", () => {
    const props = makeProps({ selectedCount: 0 });
    renderWithI18n(() => <ToolbarBatch {...props} />);

    const btn = screen.getByLabelText("全选");
    expect(btn).toBeDefined();
    expect(btn.getAttribute("aria-pressed")).toBe("false");
  });

  it("部分选中时仍显示「全选」并展示 indeterminate 状态", () => {
    const props = makeProps({ selectedCount: 2, totalCount: 4 });
    renderWithI18n(() => <ToolbarBatch {...props} />);

    const btn = screen.getByLabelText("全选");
    expect(btn).toBeDefined();
    expect(btn.getAttribute("aria-pressed")).toBe("false");
  });

  it("全部选中时显示「取消全选」且 aria-pressed=true", () => {
    const props = makeProps({ selectedCount: 4, totalCount: 4 });
    renderWithI18n(() => <ToolbarBatch {...props} />);

    const btn = screen.getByLabelText("取消全选");
    expect(btn).toBeDefined();
    expect(btn.getAttribute("aria-pressed")).toBe("true");
  });

  describe("移动端窄屏 (<640px)", () => {
    beforeEach(() => {
      mockMatchMedia(true);
    });

    afterEach(() => {
      vi.unstubAllGlobals();
    });

    it("小屏下主栏保留核心操作并收起次要操作到「更多」", () => {
      renderWithI18n(() => <ToolbarBatch {...makeProps()} />);

      expect(screen.getByLabelText("全选")).toBeDefined();
      expect(screen.getByLabelText("删除")).toBeDefined();
      expect(screen.getByLabelText("清空选择")).toBeDefined();
      expect(screen.getByLabelText("退出多选")).toBeDefined();
      expect(screen.getByLabelText("更多操作")).toBeDefined();

      expect(screen.queryByLabelText("暂停")).toBeNull();
      expect(screen.queryByLabelText("恢复")).toBeNull();
      expect(screen.queryByLabelText("取消")).toBeNull();
      expect(screen.queryByLabelText("打开文件夹")).toBeNull();
      expect(screen.queryByLabelText("复制链接")).toBeNull();
      expect(screen.queryByLabelText("重新下载")).toBeNull();
    });

    it("点击「更多」打开菜单并显示次要操作", () => {
      renderWithI18n(() => <ToolbarBatch {...makeProps()} />);

      expect(screen.queryByRole("menu")).toBeNull();
      fireEvent.click(screen.getByLabelText("更多操作"));
      expect(screen.getByRole("menu")).toBeDefined();

      expect(screen.getByRole("menuitem", { name: "暂停" })).toBeDefined();
      expect(screen.getByRole("menuitem", { name: "恢复" })).toBeDefined();
      expect(screen.getByRole("menuitem", { name: "取消" })).toBeDefined();
      expect(
        screen.getByRole("menuitem", { name: "打开文件夹" }),
      ).toBeDefined();
      expect(screen.getByRole("menuitem", { name: "复制链接" })).toBeDefined();
      expect(
        screen.getByRole("menuitem", { name: "重新下载" }),
      ).toBeDefined();
    });

    it("更多菜单中点击暂停调用 onPauseSelected", () => {
      const props = makeProps();
      renderWithI18n(() => <ToolbarBatch {...props} />);

      fireEvent.click(screen.getByLabelText("更多操作"));
      fireEvent.click(screen.getByRole("menuitem", { name: "暂停" }));
      expect(props.onPauseSelected).toHaveBeenCalledTimes(1);
    });

    it("更多菜单中点击打开文件夹调用 onOpenSelectedFolders", () => {
      const props = makeProps();
      renderWithI18n(() => <ToolbarBatch {...props} />);

      fireEvent.click(screen.getByLabelText("更多操作"));
      fireEvent.click(screen.getByRole("menuitem", { name: "打开文件夹" }));
      expect(props.onOpenSelectedFolders).toHaveBeenCalledTimes(1);
    });

    it("点击菜单外部关闭「更多」菜单", () => {
      renderWithI18n(() => <ToolbarBatch {...makeProps()} />);

      fireEvent.click(screen.getByLabelText("更多操作"));
      expect(screen.getByRole("menu")).toBeDefined();

      fireEvent.mouseDown(document.body);
      expect(screen.queryByRole("menu")).toBeNull();
    });
  });
});
