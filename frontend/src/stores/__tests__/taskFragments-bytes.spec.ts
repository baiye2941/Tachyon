import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  loadTaskFragments,
  mergeFragmentDelta,
  getTaskFragmentData,
  clearTaskFragments,
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
});
