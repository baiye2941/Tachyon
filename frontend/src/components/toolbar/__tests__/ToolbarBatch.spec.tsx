import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../../i18n";
import type { JSX } from "solid-js";
import ToolbarBatch from "../ToolbarBatch";

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
});
