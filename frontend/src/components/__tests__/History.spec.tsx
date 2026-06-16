import { describe, it, expect, afterEach, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@solidjs/testing-library";
import type { HistoryRecord } from "../../stores/history";

const STORAGE_KEY = "tachyon:download_history";

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
    ...overrides,
  };
}

async function renderPanel(
  overrides: Record<string, unknown> = {},
  records: HistoryRecord[] = [],
) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(records));
  vi.resetModules();
  const { default: HistoryPanel } = await import("../HistoryPanel");
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
    vi.resetModules();
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

  it("点击打开目录触发 onOpenFolder", async () => {
    const onOpenFolder = vi.fn();
    await renderPanel({ onOpenFolder }, [
      makeRecord({ id: "a", fileName: "a.zip" }),
    ]);
    fireEvent.click(screen.getByLabelText("打开目录 a.zip"));
    expect(onOpenFolder).toHaveBeenCalledWith("a");
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
});
