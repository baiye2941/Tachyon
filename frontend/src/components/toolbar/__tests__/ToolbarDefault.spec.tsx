import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, cleanup } from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../../i18n";
import type { JSX } from "solid-js";
import { regular as P } from "../../iconPaths";
import ToolbarDefault from "../ToolbarDefault";

const renderWithI18n = (ui: () => JSX.Element) =>
  render(() => <I18nProvider i18n={i18n}>{ui()}</I18nProvider>);

const makeProps = (
  overrides: Partial<Parameters<typeof ToolbarDefault>[0]> = {},
) => ({
  searchQuery: "",
  onSearchChange: vi.fn(),
  filters: [],
  onRemoveFilter: vi.fn(),
  isMultiSelectMode: false,
  onToggleMultiSelect: vi.fn(),
  selectedCount: 0,
  totalCount: 0,
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
  groupBy: "status" as const,
  onToggleGroupBy: vi.fn(),
  ...overrides,
});

/** 取按钮内首个 svg path 的 d 属性,作为图标指纹比对 */
const iconPathOf = (btn: HTMLElement) =>
  btn.querySelector("svg path")?.getAttribute("d");

// 分组按钮 aria-label 来自 zh-CN "切换为{mode}视图" 插值(mode = "平铺视图"),
// 现状渲染结果为「切换为平铺视图视图」,此处按现状字面量定位
const DENSITY_LABEL = "宽松列表";
const GROUP_BY_LABEL = "切换为平铺视图视图";

describe("ToolbarDefault 默认工具栏", () => {
  afterEach(() => {
    cleanup();
  });

  it("密度与分组切换按钮渲染不同图标(回归:曾同用 ListBulletsIcon 撞车)", () => {
    renderWithI18n(() => <ToolbarDefault {...makeProps()} />);

    const densityBtn = screen.getByLabelText(DENSITY_LABEL);
    const groupByBtn = screen.getByLabelText(GROUP_BY_LABEL);

    const densityPath = iconPathOf(densityBtn);
    const groupByPath = iconPathOf(groupByBtn);

    expect(densityPath).toBeTruthy();
    expect(groupByPath).toBeTruthy();
    // 核心回归断言:两个 icon-only 按钮的图标 path 必须不同
    expect(groupByPath).not.toBe(densityPath);
    // 密度侧守卫:保持 listBullets 不变,防止修复时误改密度按钮
    expect(densityPath).toBe(P.listBullets);
  });

  it("密度/分组按钮 aria-label 与 title 语义不变(字面量守卫)", () => {
    renderWithI18n(() => <ToolbarDefault {...makeProps()} />);

    const densityBtn = screen.getByLabelText(DENSITY_LABEL);
    expect(densityBtn.getAttribute("title")).toBe("宽松列表");

    const groupByBtn = screen.getByLabelText(GROUP_BY_LABEL);
    expect(groupByBtn.getAttribute("title")).toBe("平铺视图");
  });

  it("搜索框挂载 glow-border 流光类(hover 边框流光)", () => {
    const { container } = renderWithI18n(() => (
      <ToolbarDefault {...makeProps()} />
    ));

    const searchInput = container.querySelector("input.input");
    const wrapper = searchInput?.closest(".glow-border");
    expect(wrapper).not.toBeNull();
    expect(wrapper?.contains(searchInput!)).toBe(true);
  });
});
