import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  loadTaskFragments,
  mergeFragmentDelta,
  getTaskFragmentData,
  clearTaskFragments,
} from "../taskFragments";
import { api } from "../../api/invoke";
import type { TaskFragmentsView } from "../../types";

// mock invoke 避免真实 Tauri 调用
vi.mock("../../api/invoke", () => ({
  api: {
    getTaskFragments: vi.fn(),
  },
}));

const mockGetTaskFragments = vi.mocked(api.getTaskFragments);

describe("loadTaskFragments 快照合并", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    clearTaskFragments("t-race");
  });

  it("重拉快照不覆盖 await 窗口内的 started delta", async () => {
    // 首拉建立 entry:downloading [5, 6]
    mockGetTaskFragments.mockResolvedValueOnce({
      total: 8,
      doneIndices: [],
      downloadingIndices: [5, 6],
    });
    await loadTaskFragments("t-race");

    // 二拉挂起,制造 await 窗口
    let resolveSecond!: (view: TaskFragmentsView) => void;
    mockGetTaskFragments.mockImplementationOnce(
      () =>
        new Promise<TaskFragmentsView>((resolve) => {
          resolveSecond = resolve;
        }),
    );
    const pending = loadTaskFragments("t-race");

    // 窗口内:6 完成、7 开始。
    // 旧实现 downloadingSet 以快照为准([5]),7 被覆盖丢失;
    // 正确语义:snapshot ∪ (本地 downloadingSet − mergedDone)
    mergeFragmentDelta("t-race", [6], [7], []);
    resolveSecond({ total: 8, doneIndices: [6], downloadingIndices: [5] });
    await pending;

    const data = getTaskFragmentData("t-race")!;
    // completed 并集兜底:快照 [6] ∪ 本地 [6]
    expect(data.doneSet.has(6)).toBe(true);
    // 快照下载中集合保留
    expect(data.downloadingSet.has(5)).toBe(true);
    // 关键:await 窗口内的 started(7)不得被快照覆盖
    expect(data.downloadingSet.has(7)).toBe(true);
    // 已完成的分片不得残留于下载中集合
    expect(data.downloadingSet.has(6)).toBe(false);
  });

  it("快照 downloadingIndices 缺失时回退为空集合", async () => {
    mockGetTaskFragments.mockResolvedValueOnce({
      total: 4,
      doneIndices: [0],
      downloadingIndices: undefined,
    } as unknown as TaskFragmentsView);
    await loadTaskFragments("t-race");
    const data = getTaskFragmentData("t-race")!;
    expect(data.doneSet.has(0)).toBe(true);
    expect(data.downloadingSet.size).toBe(0);
  });
});
