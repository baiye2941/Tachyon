import { describe, it, expect, vi, afterEach, beforeEach } from "vitest";
import { render, cleanup, fireEvent } from "@solidjs/testing-library";
import { useGlobalKeyboard } from "../useGlobalKeyboard";
import {
  openNewTaskModal,
  openCommandPalette,
  openShortcutHelp,
  toggleSidebar,
  openView,
} from "../../stores/ui";
import { pauseAll, resumeAll, deleteSelected } from "../../stores/batchActions";
import { resetAllShortcuts } from "../../stores/shortcuts";
import { selectAll, deselectAll, selectedCount } from "../../stores/selection";

vi.mock("../../stores/ui", () => ({
  openNewTaskModal: vi.fn(),
  openCommandPalette: vi.fn(),
  openShortcutHelp: vi.fn(),
  toggleSidebar: vi.fn(),
  openView: vi.fn(),
  $ui: {
    commandPaletteOpen: () => false,
    shortcutHelpOpen: () => false,
  },
}));

vi.mock("../../stores/batchActions", () => ({
  pauseAll: vi.fn(),
  resumeAll: vi.fn(),
  deleteSelected: vi.fn(),
}));

function TestHarness() {
  useGlobalKeyboard();
  return <input aria-label="url" />;
}

describe("useGlobalKeyboard", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    resetAllShortcuts();
    deselectAll();
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it("Ctrl+N 打开新建下载", () => {
    render(() => <TestHarness />);

    fireEvent.keyDown(window, { key: "n", ctrlKey: true });

    expect(openNewTaskModal).toHaveBeenCalledTimes(1);
  });

  it("Cmd+N 在 macOS 下打开新建下载", () => {
    vi.stubGlobal("navigator", {
      userAgentData: { platform: "macOS" },
      userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)",
    });
    render(() => <TestHarness />);

    fireEvent.keyDown(window, { key: "N", metaKey: true });

    expect(openNewTaskModal).toHaveBeenCalledTimes(1);
  });

  it("输入框内 Ctrl+N 不拦截编辑行为", () => {
    render(() => <TestHarness />);
    const input = document.querySelector("input")!;

    fireEvent.keyDown(input, { key: "n", ctrlKey: true });

    expect(openNewTaskModal).not.toHaveBeenCalled();
  });

  it("Ctrl+K 打开命令面板", () => {
    render(() => <TestHarness />);

    fireEvent.keyDown(window, { key: "k", ctrlKey: true });

    expect(openCommandPalette).toHaveBeenCalledTimes(1);
  });

  it("Windows 平台 Ctrl+K 命中且 Meta+K 不命中", () => {
    vi.stubGlobal("navigator", { platform: "Win32" });
    render(() => <TestHarness />);

    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    expect(openCommandPalette).toHaveBeenCalledTimes(1);

    fireEvent.keyDown(window, { key: "k", metaKey: true });
    expect(openCommandPalette).toHaveBeenCalledTimes(1);
  });

  it("Ctrl+B 切换侧边栏", () => {
    render(() => <TestHarness />);

    fireEvent.keyDown(window, { key: "b", ctrlKey: true });

    expect(toggleSidebar).toHaveBeenCalledTimes(1);
  });

  it("Ctrl+Shift+P 暂停全部任务", () => {
    render(() => <TestHarness />);

    fireEvent.keyDown(window, { key: "P", ctrlKey: true, shiftKey: true });

    expect(pauseAll).toHaveBeenCalledTimes(1);
  });

  it("Ctrl+Shift+R 恢复全部任务", () => {
    render(() => <TestHarness />);

    fireEvent.keyDown(window, { key: "R", ctrlKey: true, shiftKey: true });

    expect(resumeAll).toHaveBeenCalledTimes(1);
  });

  it("Ctrl+, 打开设置视图", () => {
    render(() => <TestHarness />);

    fireEvent.keyDown(window, { key: ",", ctrlKey: true });

    expect(openView).toHaveBeenCalledWith("settings");
  });

  it("? 打开快捷键帮助", () => {
    render(() => <TestHarness />);

    fireEvent.keyDown(window, { key: "?" });

    expect(openShortcutHelp).toHaveBeenCalledTimes(1);
  });

  it("Delete 键调用 deleteSelected", () => {
    selectAll(["task-1"]);
    render(() => <TestHarness />);

    fireEvent.keyDown(window, { key: "Delete" });

    expect(deleteSelected).toHaveBeenCalledTimes(1);
  });

  it("无选中时 Delete 键不调用 deleteSelected", () => {
    render(() => <TestHarness />);

    fireEvent.keyDown(window, { key: "Delete" });

    expect(deleteSelected).not.toHaveBeenCalled();
  });

  it("Ctrl+A 切换全选/取消全选", () => {
    selectAll(["task-1"]);
    render(() => <TestHarness />);

    fireEvent.keyDown(window, { key: "A", ctrlKey: true });

    // 测试环境 $taskFilter 为空,Ctrl+A 会取消全选
    expect(selectedCount()).toBe(0);
  });
});
