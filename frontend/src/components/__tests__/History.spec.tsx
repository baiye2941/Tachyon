import { describe, it, expect, afterEach, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@solidjs/testing-library";
import HistoryPanel from "../HistoryPanel";
import {
  clearHistory,
  loadHistoryRecords,
  type HistoryRecord,
} from "../../stores/history";
import * as historyStore from "../../stores/history";

// mock confirm store:批量删除测试需要控制 requestConfirm 返回值
const mockRequestConfirm = vi.fn();
vi.mock("../../stores/confirm", () => ({
  requestConfirm: (...args: unknown[]) => mockRequestConfirm(...args),
}));

function makeRecord(overrides: Partial<HistoryRecord> = {}): HistoryRecord {
  return {
    id: `id-${Math.random().toString(36).slice(2)}`,
    url: "https://example.com/file.zip",
    fileName: "file.zip",
    fileSize: 1024 * 1024,
    status: "completed",
    duration: 5000,
    avgSpeed: 204800,
    completedAt: "2026-05-30T10:00:00Z",
    savePath: "",
    ...overrides,
  };
}

function renderPanel(
  overrides: Record<string, unknown> = {},
  records: HistoryRecord[] = [],
) {
  loadHistoryRecords(records);
  return render(() => (
    <HistoryPanel
      visible={true}
      tasks={[]}
      onClose={() => {}}
      onOpenFolder={() => {}}
      onRedownload={() => {}}
      onDeleteRecord={() => {}}
      {...overrides}
    />
  ));
}

describe("HistoryPanel 历史记录面板", () => {
  beforeEach(() => {
    localStorage.clear();
    clearHistory();
    mockRequestConfirm.mockReset();
  });

  afterEach(() => {
    cleanup();
  });

  it('标题显示 "下载历史"', async () => {
    await renderPanel();
    expect(screen.getByText("下载历史")).toBeDefined();
  });

  it("渲染所有状态的历史记录", async () => {
    await renderPanel({}, [
      makeRecord({ id: "a", fileName: "a.zip", status: "completed" }),
      makeRecord({ id: "b", fileName: "b.zip", status: "failed" }),
      makeRecord({ id: "c", fileName: "c.zip", status: "cancelled" }),
    ]);
    expect(screen.getByText("a.zip")).toBeDefined();
    expect(screen.getByText("b.zip")).toBeDefined();
    expect(screen.getByText("c.zip")).toBeDefined();
  });

  it("搜索历史记录按文件名过滤", async () => {
    await renderPanel({}, [
      makeRecord({ id: "a", fileName: "a.zip" }),
      makeRecord({ id: "c", fileName: "c.zip" }),
    ]);
    fireEvent.input(screen.getByPlaceholderText("搜索历史记录..."), {
      target: { value: "a." },
    });
    expect(screen.getByText("a.zip")).toBeDefined();
    expect(screen.queryByText("c.zip")).toBeNull();
  });

  it("点击删除记录触发 onDeleteRecord", async () => {
    const onDeleteRecord = vi.fn();
    await renderPanel({ onDeleteRecord }, [
      makeRecord({ id: "a", fileName: "a.zip" }),
    ]);
    fireEvent.click(screen.getByLabelText("删除记录 a.zip"));
    expect(onDeleteRecord).toHaveBeenCalledWith("a");
  });

  it("点击重新下载触发 onRedownload 并传回任务", async () => {
    const onRedownload = vi.fn();
    await renderPanel({ onRedownload }, [
      makeRecord({
        id: "a",
        fileName: "a.zip",
        url: "https://example.com/a.zip",
      }),
    ]);
    fireEvent.click(screen.getByLabelText("重新下载 a.zip"));
    expect(onRedownload).toHaveBeenCalledWith(
      expect.objectContaining({
        id: "a",
        fileName: "a.zip",
        url: "https://example.com/a.zip",
      }),
    );
  });

  it("点击打开目录触发 onOpenFolder 并传回父目录", async () => {
    const onOpenFolder = vi.fn();
    await renderPanel({ onOpenFolder }, [
      makeRecord({
        id: "a",
        fileName: "a.zip",
        savePath: "D:\\downloads\\a.zip",
      }),
    ]);
    fireEvent.click(screen.getByLabelText("打开目录 a.zip"));
    expect(onOpenFolder).toHaveBeenCalledWith("D:\\downloads");
  });

  it("savePath 为空时点击打开目录传回空字符串", async () => {
    const onOpenFolder = vi.fn();
    await renderPanel({ onOpenFolder }, [
      makeRecord({ id: "a", fileName: "a.zip", savePath: "" }),
    ]);
    fireEvent.click(screen.getByLabelText("打开目录 a.zip"));
    expect(onOpenFolder).toHaveBeenCalledWith("");
  });

  it("批量模式:切换后显示复选框,点击记录切换选中", async () => {
    await renderPanel({}, [
      makeRecord({ id: "a", fileName: "a.zip" }),
      makeRecord({ id: "b", fileName: "b.zip" }),
    ]);
    // 进入批量模式
    fireEvent.click(screen.getByLabelText("批量选择"));
    // 选中第一条
    fireEvent.click(screen.getByLabelText("选择记录 a.zip"));
    expect(screen.getByText("已选 1 项")).toBeDefined();
  });

  it("批量模式:全选按钮选中所有可见记录", async () => {
    await renderPanel({}, [
      makeRecord({ id: "a", fileName: "a.zip" }),
      makeRecord({ id: "b", fileName: "b.zip" }),
    ]);
    fireEvent.click(screen.getByLabelText("批量选择"));
    fireEvent.click(screen.getByText("全选"));
    expect(screen.getByText("已选 2 项")).toBeDefined();
  });

  it("批量模式:删除选中触发确认对话框", async () => {
    mockRequestConfirm.mockResolvedValue({ ok: true, deleteLocalFile: false });
    const onDeleteRecord = vi.fn();
    await renderPanel({ onDeleteRecord }, [
      makeRecord({ id: "a", fileName: "a.zip" }),
      makeRecord({ id: "b", fileName: "b.zip" }),
    ]);
    fireEvent.click(screen.getByLabelText("批量选择"));
    fireEvent.click(screen.getByText("全选"));
    fireEvent.click(screen.getByText("删除选中"));
    // 确认对话框应被调用，并携带删除本地文件选项
    await vi.waitFor(() => {
      expect(mockRequestConfirm).toHaveBeenCalledWith(
        expect.objectContaining({ showDeleteLocalFileOption: true }),
      );
    });
    // 确认后逐条删除，且跳过二次确认
    await vi.waitFor(() => {
      expect(onDeleteRecord).toHaveBeenCalledTimes(2);
    });
    expect(onDeleteRecord).toHaveBeenCalledWith("a", {
      skipConfirm: true,
      deleteLocalFile: false,
    });
    expect(onDeleteRecord).toHaveBeenCalledWith("b", {
      skipConfirm: true,
      deleteLocalFile: false,
    });
    mockRequestConfirm.mockReset();
  });

  it("批量模式:删除选中可传递 deleteLocalFile=true", async () => {
    mockRequestConfirm.mockResolvedValue({ ok: true, deleteLocalFile: true });
    const onDeleteRecord = vi.fn();
    await renderPanel({ onDeleteRecord }, [
      makeRecord({ id: "a", fileName: "a.zip" }),
    ]);
    fireEvent.click(screen.getByLabelText("批量选择"));
    fireEvent.click(screen.getByLabelText("选择记录 a.zip"));
    fireEvent.click(screen.getByText("删除选中"));
    await vi.waitFor(() => {
      expect(onDeleteRecord).toHaveBeenCalledWith("a", {
        skipConfirm: true,
        deleteLocalFile: true,
      });
    });
    mockRequestConfirm.mockReset();
  });

  it("统计卡片使用 panel-surface 而非 glass", async () => {
    const { container } = await renderPanel({}, [
      makeRecord({ fileName: "a.zip" }),
    ]);
    const panels = container.querySelectorAll(".panel-surface");
    expect(panels.length).toBeGreaterThan(0);
    expect(container.querySelectorAll(".glass").length).toBe(0);
  });

  it("没有历史记录时显示空状态", async () => {
    await renderPanel();
    expect(screen.getByText("暂无历史记录")).toBeDefined();
  });

  it("显示文件大小和已完成状态", async () => {
    await renderPanel({}, [
      makeRecord({ fileName: "a.zip", fileSize: 1024 * 1024 }),
    ]);
    expect(screen.getAllByText("1.0 MB").length).toBeGreaterThan(0);
    expect(screen.getAllByText(/已完成/).length).toBeGreaterThan(0);
  });

  it("统计基于历史记录", async () => {
    await renderPanel({}, [
      makeRecord({
        fileName: "a.zip",
        fileSize: 1024 * 1024,
        avgSpeed: 204800,
      }),
      makeRecord({
        fileName: "b.zip",
        fileSize: 512 * 1024,
        avgSpeed: 102400,
        status: "failed",
      }),
    ]);
    expect(screen.getByText("1.5 MB")).toBeDefined();
    expect(screen.getByText("2")).toBeDefined();
  });

  it("时间范围切换正常", async () => {
    await renderPanel({}, [
      makeRecord({
        id: "old",
        fileName: "old.zip",
        completedAt: "2026-01-01T10:00:00Z",
      }),
      makeRecord({
        id: "recent",
        fileName: "recent.zip",
        completedAt: new Date().toISOString(),
      }),
    ]);
    expect(screen.getByText("old.zip")).toBeDefined();
    expect(screen.getByText("recent.zip")).toBeDefined();

    fireEvent.click(screen.getByText("近7天"));
    expect(screen.queryByText("old.zip")).toBeNull();
    expect(screen.getByText("recent.zip")).toBeDefined();

    fireEvent.click(screen.getByText("近30天"));
    expect(screen.queryByText("old.zip")).toBeNull();
    expect(screen.getByText("recent.zip")).toBeDefined();

    fireEvent.click(screen.getByText("全部"));
    expect(screen.getByText("old.zip")).toBeDefined();
    expect(screen.getByText("recent.zip")).toBeDefined();
  });

  it("趋势图渲染不崩溃", async () => {
    await renderPanel({}, [
      makeRecord({
        fileName: "a.zip",
        fileSize: 1024 * 1024,
        completedAt: new Date().toISOString(),
      }),
    ]);
    expect(screen.getByText("下载量趋势")).toBeDefined();
  });

  describe("性能优化", () => {
    it("批量模式切换不触发统计重算", async () => {
      const spy = vi.spyOn(historyStore, "getHistoryStatsForRecords");
      await renderPanel({}, [
        makeRecord({ id: "a", fileName: "a.zip" }),
        makeRecord({ id: "b", fileName: "b.zip" }),
      ]);
      spy.mockClear();

      fireEvent.click(screen.getByLabelText("批量选择"));
      await new Promise<void>((resolve) => setTimeout(resolve, 0));

      expect(spy).not.toHaveBeenCalled();
      spy.mockRestore();
    });

    it("列表使用 keyed For：前置记录后原有行节点保持不变", async () => {
      const a = makeRecord({ id: "a", fileName: "a.zip" });
      const b = makeRecord({ id: "b", fileName: "b.zip" });
      const { container } = await renderPanel({}, [a, b]);

      const aElement = screen.getByText("a.zip");
      const bElement = screen.getByText("b.zip");

      const c = makeRecord({ id: "c", fileName: "c.zip" });
      loadHistoryRecords([c, a, b]);
      await new Promise<void>((resolve) => setTimeout(resolve, 0));

      const rows = container.querySelectorAll(".hover-row");
      expect(rows.length).toBe(3);
      expect(rows[0]?.textContent).toContain("c.zip");
      expect(rows[1]?.textContent).toContain("a.zip");
      expect(rows[2]?.textContent).toContain("b.zip");

      expect(screen.getByText("a.zip")).toBe(aElement);
      expect(screen.getByText("b.zip")).toBe(bElement);
    });
  });
});
