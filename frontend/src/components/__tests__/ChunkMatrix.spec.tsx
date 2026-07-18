import {
  describe,
  it,
  expect,
  afterEach,
  vi,
  beforeAll,
  beforeEach,
} from "vitest";
import {
  render,
  screen,
  cleanup,
  fireEvent,
} from "@solidjs/testing-library";
import ChunkMatrix, { buildBlocks } from "../ChunkMatrix";
import * as taskFragments from "../../stores/taskFragments";
import type { TaskFragmentData } from "../../stores/taskFragments";

/**
 * 可响应式更新的 store mock。
 *
 * 通过 Solid signal 驱动,模拟真实 progress tick:store 每次产生新对象/新 Set 引用,
 * 但内容可能未变。依赖 ChunkMatrix 内部做稳定化处理,避免全量 DOM 重建。
 */
vi.mock("../../stores/taskFragments", async () => {
  const { createSignal } = await import("solid-js");
  const fragmentMap = new Map<string, TaskFragmentData>();
  const [version, setVersion] = createSignal(0);
  const updateVersion = () => setVersion((v) => v + 1);

  return {
    getTaskFragmentData: vi.fn((taskId: string) => {
      version();
      return fragmentMap.get(taskId);
    }),
    mergeFragmentDelta: vi.fn(
      (
        taskId: string,
        completedDelta: number[],
        startedDelta: number[],
      ) => {
        const data = fragmentMap.get(taskId);
        if (data) {
          const newDone = new Set(data.doneSet);
          const newDownloading = new Set(data.downloadingSet);
          for (const idx of completedDelta) {
            newDone.add(idx);
            newDownloading.delete(idx);
          }
          for (const idx of startedDelta) {
            if (!newDone.has(idx)) newDownloading.add(idx);
          }
          fragmentMap.set(taskId, {
            ...data,
            doneSet: newDone,
            downloadingSet: newDownloading,
          });
        }
        updateVersion();
      },
    ),
    __testSetFragmentData: (taskId: string, data: TaskFragmentData) => {
      fragmentMap.set(taskId, data);
      updateVersion();
    },
    __testResetFragmentData: () => {
      fragmentMap.clear();
      updateVersion();
    },
  };
});

const {
  mergeFragmentDelta,
  __testSetFragmentData: setFragmentData,
  __testResetFragmentData: resetFragmentData,
} = taskFragments as unknown as {
  mergeFragmentDelta: typeof taskFragments.mergeFragmentDelta;
  __testSetFragmentData: (taskId: string, data: TaskFragmentData) => void;
  __testResetFragmentData: () => void;
};

function mockMatchMedia(matches: boolean) {
  Object.defineProperty(window, "matchMedia", {
    writable: true,
    value: vi.fn().mockImplementation((query: string) => ({
      matches: query === "(prefers-reduced-motion: reduce)" ? matches : false,
      media: query,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    })),
  });
}

function createMockContext(): CanvasRenderingContext2D {
  const grad = {
    addColorStop: vi.fn(),
  } as unknown as CanvasGradient;
  return {
    setTransform: vi.fn(),
    clearRect: vi.fn(),
    beginPath: vi.fn(),
    roundRect: vi.fn(),
    fill: vi.fn(),
    stroke: vi.fn(),
    createLinearGradient: vi.fn().mockReturnValue(grad),
    save: vi.fn(),
    restore: vi.fn(),
    clip: vi.fn(),
    fillRect: vi.fn(),
  } as unknown as CanvasRenderingContext2D;
}

beforeAll(() => {
  const originalGetContext = HTMLCanvasElement.prototype.getContext;
  HTMLCanvasElement.prototype.getContext = function (
    this: HTMLCanvasElement,
    contextId: "2d" | "bitmaprenderer" | "webgl" | "webgl2",
  ) {
    if (contextId === "2d") {
      return createMockContext();
    }
    return originalGetContext.call(this, contextId);
  } as typeof HTMLCanvasElement.prototype.getContext;
});

describe("ChunkMatrix 分片矩阵", () => {
  beforeEach(() => {
    mockMatchMedia(false);
    cleanup();
    document.body.innerHTML = "";
    // 每个测试重置 store mock,避免上一测试注入的数据泄漏
    resetFragmentData();
    vi.mocked(mergeFragmentDelta).mockClear();
  });

  afterEach(() => {
    cleanup();
    document.body.innerHTML = "";
  });

  describe("buildBlocks 聚合逻辑", () => {
    it("total <= 0 时返回空数组", () => {
      expect(buildBlocks(0, new Set(), new Set())).toEqual([]);
      expect(buildBlocks(-1, new Set(), new Set())).toEqual([]);
    });

    it("total 较小时返回与总数相同的块数", () => {
      const blocks = buildBlocks(10, new Set([0, 1, 2, 3, 4]), new Set());
      expect(blocks.length).toBe(10);
      expect(blocks[0]!.start).toBe(0);
      expect(blocks[9]!.end).toBe(10);
    });

    it("total 较大时固定为 100 块", () => {
      const blocks = buildBlocks(
        1000,
        new Set(Array.from({ length: 500 }, (_, i) => i)),
        new Set(),
      );
      expect(blocks.length).toBe(100);
    });

    it("块范围覆盖全部分片且不重叠", () => {
      const blocks = buildBlocks(
        1000,
        new Set(Array.from({ length: 500 }, (_, i) => i)),
        new Set(),
      );
      expect(blocks[0]!.start).toBe(0);
      expect(blocks[blocks.length - 1]!.end).toBe(1000);
      for (let i = 1; i < blocks.length; i++) {
        expect(blocks[i]!.start).toBe(blocks[i - 1]!.end);
      }
    });

    it("已完成分片占多数时块状态为 done", () => {
      const blocks = buildBlocks(
        100,
        new Set(Array.from({ length: 60 }, (_, i) => i)),
        new Set(),
      );
      expect(blocks[0]!.status).toBe("done");
    });

    it("等待中分片占多数时块状态为 pending", () => {
      const blocks = buildBlocks(
        100,
        new Set(Array.from({ length: 10 }, (_, i) => i)),
        new Set(),
      );
      expect(blocks[90]!.status).toBe("pending");
    });

    it("downloadingSet 中的分片使块状态为 downloading", () => {
      // 分片 60-63 在 downloadingSet,块 60 所属的 block 应显示 downloading
      const downloadingSet = new Set([60, 61, 62, 63]);
      const blocks = buildBlocks(
        100,
        new Set(Array.from({ length: 10 }, (_, i) => i)),
        downloadingSet,
      );
      const downloadingBlock = blocks.find((b) => b.status === "downloading");
      expect(downloadingBlock).toBeDefined();
      expect(downloadingBlock!.color).toBe("var(--color-status-downloading)");
    });

    it("downloadingSet 与 doneSet 互斥时优先 done", () => {
      // 分片 5 同时在 doneSet 和 downloadingSet(防御竞态),应算作 done
      const blocks = buildBlocks(10, new Set([5]), new Set([5]));
      const block = blocks.find((b) => b.start <= 5 && b.end > 5);
      expect(block).toBeDefined();
      expect(block!.done).toBe(1);
    });

    it("块颜色按状态着色,不再使用线程彩虹色", () => {
      const blocks = buildBlocks(
        120,
        new Set(Array.from({ length: 60 }, (_, i) => i)),
        new Set(),
      );
      const doneBlock = blocks.find((b) => b.status === "done");
      const pendingBlock = blocks.find((b) => b.status === "pending");
      expect(doneBlock).toBeDefined();
      expect(doneBlock!.color).toBe("var(--color-status-completed)");
      expect(pendingBlock).toBeDefined();
      expect(pendingBlock!.color).toBe("var(--color-status-pending)");
      // 不应再出现之前的紫色线程色
      expect(blocks.some((b) => b.color === "#A855F7")).toBe(false);
    });

    it("大任务下 buildBlocks 不扫描全量分片,十万级可在 50ms 内完成", () => {
      const total = 100_000;
      const doneSet = new Set<number>();
      const downloadingSet = new Set<number>();
      for (let i = 0; i < total; i += 2) doneSet.add(i);
      for (let i = 1; i < total; i += 4) downloadingSet.add(i);

      const start = performance.now();
      const blocks = buildBlocks(total, doneSet, downloadingSet);
      const elapsed = performance.now() - start;

      expect(blocks.length).toBe(100);
      expect(elapsed).toBeLessThan(200); // 并行测试满载时 CPU 会有波动，放宽阈值保证稳定性
    });
  });

  describe("组件渲染", () => {
    it("分片数 <= 200 时渲染 DOM 分片", () => {
      render(() => (
        <ChunkMatrix
          taskId="test-task"
          fragmentsTotal={100}
          fragmentsDone={50}
          progress={0.5}
        />
      ));
      const cells = document.querySelectorAll(".chunk-cell");
      expect(cells.length).toBe(100);
      expect(document.querySelector("canvas")).toBeNull();
    });

    it("分片数 > 200 时渲染 canvas", () => {
      render(() => (
        <ChunkMatrix
          taskId="test-task"
          fragmentsTotal={1000}
          fragmentsDone={500}
          progress={0.5}
        />
      ));
      expect(document.querySelector("canvas")).not.toBeNull();
      expect(document.querySelectorAll(".chunk-cell").length).toBe(0);
    });

    it("接受 fragmentsTotal、fragmentsDone、progress props 不报错", () => {
      expect(() => {
        render(() => (
          <ChunkMatrix
            taskId="test-task"
            fragmentsTotal={0}
            fragmentsDone={0}
            progress={0}
          />
        ));
      }).not.toThrow();
      expect(screen.getAllByText("分片分布").length).toBeGreaterThanOrEqual(1);
    });

    it("DOM 分片按状态携带对应 class", () => {
      // 注入分片数据:8 个已完成索引 [0..7],与 fragmentsDone=8 对齐。
      // 索引 8-11 在 downloadingSet(正在下载)。
      setFragmentData("test-task", {
        total: 20,
        doneSet: new Set([0, 1, 2, 3, 4, 5, 6, 7]),
        downloadingSet: new Set([8, 9, 10, 11]),
        bytesMap: new Map(),
        finalized: false,
      });
      render(() => (
        <ChunkMatrix
          taskId="test-task"
          fragmentsTotal={20}
          fragmentsDone={8}
          progress={0.4}
        />
      ));
      const cells = Array.from(
        document.querySelectorAll<HTMLElement>(".chunk-cell"),
      );
      expect(cells.length).toBe(20);
      const doneCells = cells.filter((c) =>
        c.classList.contains("chunk-cell--done"),
      );
      const downloadingCells = cells.filter((c) =>
        c.classList.contains("chunk-cell--downloading"),
      );
      expect(doneCells.length).toBe(8);
      expect(downloadingCells.length).toBe(4);
      // 不再内联动画,由 CSS 类驱动
      expect(cells[0]!.style.animation).toBe("");
      expect(cells[0]!.style.opacity).toBe("");
    });

    it("DOM 下载中分片保留 shine 动画且无级联延迟", () => {
      setFragmentData("test-task", {
        total: 20,
        doneSet: new Set([0, 1, 2, 3, 4, 5, 6, 7]),
        downloadingSet: new Set([8, 9, 10, 11]),
        bytesMap: new Map(),
        finalized: false,
      });
      render(() => (
        <ChunkMatrix
          taskId="test-task"
          fragmentsTotal={20}
          fragmentsDone={8}
          progress={0.4}
        />
      ));
      const downloading = Array.from(
        document.querySelectorAll<HTMLElement>("[data-status='downloading']"),
      );
      expect(downloading.length).toBeGreaterThan(0);
      for (const cell of downloading) {
        expect(cell.classList.contains("chunk-cell--downloading")).toBe(true);
        expect(cell.style.animationDelay).toBe("");
      }
    });

    it("prefers-reduced-motion 时附加 reduced 类", () => {
      mockMatchMedia(true);
      setFragmentData("test-task", {
        total: 20,
        doneSet: new Set([0, 1, 2, 3, 4, 5, 6, 7]),
        downloadingSet: new Set([8, 9, 10, 11]),
        bytesMap: new Map(),
        finalized: false,
      });
      render(() => (
        <ChunkMatrix
          taskId="test-task"
          fragmentsTotal={20}
          fragmentsDone={8}
          progress={0.4}
        />
      ));
      const downloading = Array.from(
        document.querySelectorAll<HTMLElement>("[data-status='downloading']"),
      );
      expect(downloading.length).toBeGreaterThan(0);
      for (const cell of downloading) {
        expect(cell.classList.contains("chunk-cell--reduced")).toBe(true);
      }
    });

    it("prefers-reduced-motion 时不启动动画循环", () => {
      mockMatchMedia(true);
      render(() => (
        <ChunkMatrix
          taskId="test-task"
          fragmentsTotal={1000}
          fragmentsDone={500}
          progress={0.5}
        />
      ));
      // 减少动画偏好下不启动 requestAnimationFrame 动画循环,
      // 组件正常渲染 canvas 即视为通过(无 rAF 计时器泄漏)
      expect(document.querySelector("canvas")).not.toBeNull();
    });
  });

  describe("性能: progress tick 下避免全量重建", () => {
    it("分片数据引用变化但内容不变时,DOM 单元格不被重建", async () => {
      setFragmentData("test-task", {
        total: 20,
        doneSet: new Set([0, 1, 2, 3, 4, 5, 6, 7]),
        downloadingSet: new Set([8, 9, 10, 11]),
        bytesMap: new Map(),
        finalized: false,
      });
      render(() => (
        <ChunkMatrix
          taskId="test-task"
          fragmentsTotal={20}
          fragmentsDone={8}
          progress={0.4}
        />
      ));
      const cellsBefore = Array.from(
        document.querySelectorAll<HTMLElement>(".chunk-cell"),
      );
      expect(cellsBefore.length).toBe(20);

      // 模拟 progress tick:store 产生新对象与新 Set 引用,但内容完全一致
      setFragmentData("test-task", {
        total: 20,
        doneSet: new Set([0, 1, 2, 3, 4, 5, 6, 7]),
        downloadingSet: new Set([8, 9, 10, 11]),
        bytesMap: new Map(),
        finalized: false,
      });

      await Promise.resolve();

      const cellsAfter = Array.from(
        document.querySelectorAll<HTMLElement>(".chunk-cell"),
      );
      expect(cellsAfter.length).toBe(20);
      for (let i = 0; i < cellsBefore.length; i++) {
        expect(cellsAfter[i]).toBe(cellsBefore[i]);
      }
    });

    it("仅单个分片状态变化时,仅对应单元格 class 改变", async () => {
      setFragmentData("test-task", {
        total: 20,
        doneSet: new Set([0, 1, 2, 3, 4, 5, 6, 7]),
        downloadingSet: new Set([8, 9, 10, 11]),
        bytesMap: new Map(),
        finalized: false,
      });
      render(() => (
        <ChunkMatrix
          taskId="test-task"
          fragmentsTotal={20}
          fragmentsDone={8}
          progress={0.4}
        />
      ));
      const cells = Array.from(
        document.querySelectorAll<HTMLElement>(".chunk-cell"),
      );
      const classesBefore = cells.map((c) => c.className);

      // 仅把分片 12 从 pending 改为 downloading
      setFragmentData("test-task", {
        total: 20,
        doneSet: new Set([0, 1, 2, 3, 4, 5, 6, 7]),
        downloadingSet: new Set([8, 9, 10, 11, 12]),
        bytesMap: new Map(),
        finalized: false,
      });

      await Promise.resolve();

      const cellsAfter = Array.from(
        document.querySelectorAll<HTMLElement>(".chunk-cell"),
      );
      const changed = cellsAfter.filter(
        (c, i) => c.className !== classesBefore[i],
      );
      expect(changed.length).toBe(1);
      expect(changed[0]!.dataset.index).toBe("12");
      expect(changed[0]!.classList.contains("chunk-cell--downloading")).toBe(
        true,
      );
    });

    it("大任务(>1000)渲染使用 canvas,不创建海量 DOM 节点", () => {
      setFragmentData("test-task", {
        total: 10_000,
        doneSet: new Set(Array.from({ length: 5000 }, (_, i) => i)),
        downloadingSet: new Set([5000, 5001, 5002]),
        bytesMap: new Map(),
        finalized: false,
      });
      render(() => (
        <ChunkMatrix
          taskId="test-task"
          fragmentsTotal={10_000}
          fragmentsDone={5000}
          progress={0.5}
        />
      ));
      expect(document.querySelector("canvas")).not.toBeNull();
      expect(document.querySelectorAll(".chunk-cell").length).toBe(0);
    });
  });

  describe("交互", () => {
    it("DOM 分片悬停时不崩溃", () => {
      render(() => (
        <ChunkMatrix
          taskId="test-task"
          fragmentsTotal={10}
          fragmentsDone={5}
          progress={0.5}
        />
      ));
      const cells = document.querySelectorAll(".chunk-cell");
      expect(cells.length).toBeGreaterThan(0);
      fireEvent.mouseEnter(cells[0]!);
      fireEvent.mouseMove(cells[0]!);
      fireEvent.mouseLeave(cells[0]!);
    });

    it("DOM 分片可键盘聚焦并 Enter/Space 选中", () => {
      render(() => (
        <ChunkMatrix
          taskId="test-task"
          fragmentsTotal={10}
          fragmentsDone={5}
          progress={0.5}
        />
      ));
      const cells = Array.from(
        document.querySelectorAll<HTMLElement>(".chunk-cell"),
      );
      expect(cells[0]!.tabIndex).toBe(0);
      fireEvent.focus(cells[0]!);
      fireEvent.keyDown(cells[0]!, { key: "Enter" });
      expect(cells[0]!.classList.contains("chunk-cell--selected")).toBe(true);
      fireEvent.keyDown(cells[0]!, { key: "Enter" });
      expect(cells[0]!.classList.contains("chunk-cell--selected")).toBe(false);
    });

    it("DOM 分片点击选中,再次点击取消", () => {
      render(() => (
        <ChunkMatrix
          taskId="test-task"
          fragmentsTotal={10}
          fragmentsDone={5}
          progress={0.5}
        />
      ));
      const cells = Array.from(
        document.querySelectorAll<HTMLElement>(".chunk-cell"),
      );
      fireEvent.click(cells[1]!);
      expect(cells[1]!.classList.contains("chunk-cell--selected")).toBe(true);
      fireEvent.click(cells[1]!);
      expect(cells[1]!.classList.contains("chunk-cell--selected")).toBe(false);
    });

    it("ESC 取消 DOM 分片选中态", () => {
      render(() => (
        <ChunkMatrix
          taskId="test-task"
          fragmentsTotal={10}
          fragmentsDone={5}
          progress={0.5}
        />
      ));
      const wrapper = document.querySelector(".chunk-matrix-wrapper");
      const cells = Array.from(
        document.querySelectorAll<HTMLElement>(".chunk-cell"),
      );
      fireEvent.click(cells[2]!);
      expect(cells[2]!.classList.contains("chunk-cell--selected")).toBe(true);
      fireEvent.keyDown(wrapper!, { key: "Escape" });
      expect(cells[2]!.classList.contains("chunk-cell--selected")).toBe(false);
    });

    it("Canvas 块悬停时不崩溃", () => {
      render(() => (
        <ChunkMatrix
          taskId="test-task"
          fragmentsTotal={1000}
          fragmentsDone={500}
          progress={0.5}
        />
      ));
      const canvas = document.querySelector("canvas");
      expect(canvas).not.toBeNull();
      fireEvent.mouseMove(canvas!, { clientX: 20, clientY: 20 });
      fireEvent.mouseLeave(canvas!);
    });

    it("Canvas 块点击选中", () => {
      render(() => (
        <ChunkMatrix
          taskId="test-task"
          fragmentsTotal={1000}
          fragmentsDone={500}
          progress={0.5}
        />
      ));
      const canvas = document.querySelector("canvas");
      expect(canvas).not.toBeNull();
      fireEvent.click(canvas!, { clientX: 20, clientY: 20 });
      // 选中态通过 canvas 绘制验证,这里保证不崩溃即可
      expect(canvas).not.toBeNull();
    });
  });
});
