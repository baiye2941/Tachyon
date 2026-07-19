import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  loadTaskFragments,
  mergeFragmentDelta,
  getTaskFragmentData,
  clearTaskFragments,
  clearTaskFragmentDownloading,
} from "../taskFragments";

// mock invoke 避免真实 Tauri 调用
vi.mock("../../api/invoke", () => ({
  api: {
    getTaskFragments: vi.fn().mockResolvedValue({
      total: 4,
      doneIndices: [],
      downloadingIndices: [],
    }),
  },
}));

describe("taskFragments bytesMap", () => {
  beforeEach(() => {
    clearTaskFragments("t-bytes");
  });

  it("mergeFragmentDelta 合并 fragmentBytes 到 bytesMap", async () => {
    await loadTaskFragments("t-bytes");
    mergeFragmentDelta("t-bytes", [], [0, 1], [
      { index: 0, downloaded: 256 },
      { index: 1, downloaded: 128 },
    ]);
    const data = getTaskFragmentData("t-bytes");
    expect(data).toBeDefined();
    expect(data!.bytesMap.get(0)).toBe(256);
    expect(data!.bytesMap.get(1)).toBe(128);
  });

  it("快照覆盖:第二次 merge 覆盖旧字节,不在快照中的分片被移除", async () => {
    await loadTaskFragments("t-bytes");
    mergeFragmentDelta("t-bytes", [], [0, 1], [
      { index: 0, downloaded: 100 },
      { index: 1, downloaded: 200 },
    ]);
    mergeFragmentDelta("t-bytes", [], [1], [
      { index: 1, downloaded: 250 },
    ]);
    const data = getTaskFragmentData("t-bytes");
    expect(data!.bytesMap.has(0)).toBe(false);
    expect(data!.bytesMap.get(1)).toBe(250);
  });

  it("completedDelta 的分片从 bytesMap 移除", async () => {
    await loadTaskFragments("t-bytes");
    mergeFragmentDelta("t-bytes", [], [0], [{ index: 0, downloaded: 100 }]);
    mergeFragmentDelta("t-bytes", [0], [], []);
    const data = getTaskFragmentData("t-bytes");
    expect(data!.bytesMap.has(0)).toBe(false);
    expect(data!.doneSet.has(0)).toBe(true);
  });

  it("clearTaskFragmentDownloading 同时清空 downloadingSet 与 bytesMap", async () => {
    await loadTaskFragments("t-bytes");
    mergeFragmentDelta("t-bytes", [], [0, 1], [
      { index: 0, downloaded: 100 },
      { index: 1, downloaded: 50 },
    ]);
    clearTaskFragmentDownloading("t-bytes");
    const data = getTaskFragmentData("t-bytes")!;
    expect(data.downloadingSet.size).toBe(0);
    // 字节快照必须一并清空,否则终态后充能条残留在旧进度
    expect(data.bytesMap.size).toBe(0);
    expect(data.finalized).toBe(true);
  });
});

describe("mergeFragmentDelta 性能与节流", () => {
  beforeEach(() => {
    clearTaskFragments("t-perf");
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("纯字节合并(delta 为空)时复用 doneSet/downloadingSet 引用", async () => {
    await loadTaskFragments("t-perf");
    mergeFragmentDelta("t-perf", [], [0, 1], [
      { index: 0, downloaded: 100 },
      { index: 1, downloaded: 50 },
    ]);
    const before = getTaskFragmentData("t-perf")!;
    mergeFragmentDelta("t-perf", [], [], [
      { index: 0, downloaded: 200 },
      { index: 1, downloaded: 80 },
    ]);
    const after = getTaskFragmentData("t-perf")!;
    // 无状态 delta:集合内容未变,不应克隆新 Set(避免每 tick 全量分配)
    expect(after.doneSet).toBe(before.doneSet);
    expect(after.downloadingSet).toBe(before.downloadingSet);
    // 字节快照仍覆盖式更新
    expect(after.bytesMap.get(0)).toBe(200);
    expect(after.bytesMap.get(1)).toBe(80);
  });

  it("100ms 内连续纯字节合并,中间快照被节流跳过", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    await loadTaskFragments("t-perf");
    mergeFragmentDelta("t-perf", [], [0], [{ index: 0, downloaded: 100 }]);
    // 首次纯字节合并(t=1e6)通过并记录时间戳
    mergeFragmentDelta("t-perf", [], [], [{ index: 0, downloaded: 200 }]);
    // 同窗口内的后续快照被丢弃(覆盖式合并幂等,无正确性风险)
    mergeFragmentDelta("t-perf", [], [], [{ index: 0, downloaded: 300 }]);
    expect(getTaskFragmentData("t-perf")!.bytesMap.get(0)).toBe(200);
    // 间隔满 100ms 后恢复通过
    vi.setSystemTime(1_000_101);
    mergeFragmentDelta("t-perf", [], [], [{ index: 0, downloaded: 400 }]);
    expect(getTaskFragmentData("t-perf")!.bytesMap.get(0)).toBe(400);
  });

  it("状态 delta 合并永远即时通过,不受字节节流影响", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(2_000_000);
    await loadTaskFragments("t-perf");
    mergeFragmentDelta("t-perf", [], [0, 1], [
      { index: 0, downloaded: 100 },
    ]);
    // 纯字节合并建立节流时间戳
    mergeFragmentDelta("t-perf", [], [], [{ index: 0, downloaded: 200 }]);
    // 同一 100ms 窗口内到达的 completed/started delta 必须即时生效
    mergeFragmentDelta("t-perf", [0], [], [{ index: 1, downloaded: 50 }]);
    const data = getTaskFragmentData("t-perf")!;
    expect(data.doneSet.has(0)).toBe(true);
    expect(data.bytesMap.has(0)).toBe(false);
    expect(data.bytesMap.get(1)).toBe(50);
  });
});
