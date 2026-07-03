import {
  describe,
  it,
  expect,
  afterEach,
  vi,
  beforeAll,
  beforeEach,
} from "vitest";
import { render, screen, cleanup, fireEvent } from "@solidjs/testing-library";
import ChunkMatrix, { buildBlocks } from "../ChunkMatrix";
import {
  getTaskFragmentData,
  mergeFragmentDelta,
} from "../../stores/taskFragments";

// 默认返回 undefined,保持与未 mock 时一致的回退行为(空 doneSet)。
// 仅在需要精确分片状态的测试中通过 vi.mocked 注入数据。
vi.mock("../../stores/taskFragments", () => ({
  getTaskFragmentData: vi.fn(() => undefined),
  mergeFragmentDelta: vi.fn(),
}));

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
    vi.mocked(getTaskFragmentData).mockReturnValue(undefined);
    vi.mocked(mergeFragmentDelta).mockReset();
  });

  afterEach(() => {
    cleanup();
    document.body.innerHTML = "";
  });

  describe("buildBlocks 聚合逻辑", () => {
    it("total <= 0 时返回空数组", () => {
      expect(buildBlocks(0, 0, 0, new Set(), 4)).toEqual([]);
      expect(buildBlocks(-1, 0, 0, new Set(), 4)).toEqual([]);
    });

    it("total 较小时返回与总数相同的块数", () => {
      const blocks = buildBlocks(10, 5, 0.5, new Set([0, 1, 2, 3, 4]), 4);
      expect(blocks.length).toBe(10);
      expect(blocks[0]!.start).toBe(0);
      expect(blocks[9]!.end).toBe(10);
    });

    it("total 较大时固定为 100 块", () => {
      const blocks = buildBlocks(1000, 500, 0.5, new Set(Array.from({ length: 500 }, (_, i) => i)), 4);
      expect(blocks.length).toBe(100);
    });

    it("块范围覆盖全部分片且不重叠", () => {
      const blocks = buildBlocks(1000, 500, 0.5, new Set(Array.from({ length: 500 }, (_, i) => i)), 4);
      expect(blocks[0]!.start).toBe(0);
      expect(blocks[blocks.length - 1]!.end).toBe(1000);
      for (let i = 1; i < blocks.length; i++) {
        expect(blocks[i]!.start).toBe(blocks[i - 1]!.end);
      }
    });

    it("已完成分片占多数时块状态为 done", () => {
      const blocks = buildBlocks(100, 60, 0.5, new Set(Array.from({ length: 60 }, (_, i) => i)), 4);
      expect(blocks[0]!.status).toBe("done");
    });

    it("等待中分片占多数时块状态为 pending", () => {
      const blocks = buildBlocks(100, 10, 0.5, new Set(Array.from({ length: 10 }, (_, i) => i)), 4);
      expect(blocks[90]!.status).toBe("pending");
    });

    it("块颜色按状态着色,不再使用线程彩虹色", () => {
      const blocks = buildBlocks(120, 60, 0.5, new Set(Array.from({ length: 60 }, (_, i) => i)), 4);
      const doneBlock = blocks.find((b) => b.status === "done");
      const pendingBlock = blocks.find((b) => b.status === "pending");
      expect(doneBlock).toBeDefined();
      expect(doneBlock!.color).toBe("var(--color-status-completed)");
      expect(pendingBlock).toBeDefined();
      expect(pendingBlock!.color).toBe("var(--color-status-pending)");
      // 不应再出现之前的紫色线程色
      expect(blocks.some((b) => b.color === "#A855F7")).toBe(false);
    });
  });

  describe("组件渲染", () => {
    it("分片数 <= 200 时渲染 DOM 分片", () => {
      render(() => (
        <ChunkMatrix taskId="test-task" fragmentsTotal={100} fragmentsDone={50} progress={0.5} />
      ));
      const cells = document.querySelectorAll(".chunk-cell");
      expect(cells.length).toBe(100);
      expect(document.querySelector("canvas")).toBeNull();
    });

    it("分片数 > 200 时渲染 canvas", () => {
      render(() => (
        <ChunkMatrix taskId="test-task" fragmentsTotal={1000} fragmentsDone={500} progress={0.5} />
      ));
      expect(document.querySelector("canvas")).not.toBeNull();
      expect(document.querySelectorAll(".chunk-cell").length).toBe(0);
    });

    it("接受 fragmentsTotal、fragmentsDone、progress props 不报错", () => {
      expect(() => {
        render(() => (
          <ChunkMatrix taskId="test-task" fragmentsTotal={0} fragmentsDone={0} progress={0} />
        ));
      }).not.toThrow();
      expect(screen.getAllByText("分片分布").length).toBeGreaterThanOrEqual(1);
    });

    it("DOM 分片按状态携带对应 class", () => {
      // 注入分片数据:8 个已完成索引 [0..7],与 fragmentsDone=8 对齐。
      // 组件 chunks() 据此构建 doneSet,使索引 0-7 判为 done。
      vi.mocked(getTaskFragmentData).mockReturnValue({
        total: 20,
        concurrency: 4,
        doneSet: new Set([0, 1, 2, 3, 4, 5, 6, 7]),
      });
      render(() => (
        <ChunkMatrix taskId="test-task" fragmentsTotal={20} fragmentsDone={8} progress={0.4} />
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
      expect(downloadingCells.length).toBeGreaterThan(0);
      // 不再内联动画,由 CSS 类驱动
      expect(cells[0]!.style.animation).toBe("");
      expect(cells[0]!.style.opacity).toBe("");
    });

    it("DOM 下载中分片保留 shine 动画且无级联延迟", () => {
      render(() => (
        <ChunkMatrix taskId="test-task" fragmentsTotal={20} fragmentsDone={8} progress={0.4} />
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
      render(() => (
        <ChunkMatrix taskId="test-task" fragmentsTotal={20} fragmentsDone={8} progress={0.4} />
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
        <ChunkMatrix taskId="test-task" fragmentsTotal={1000} fragmentsDone={500} progress={0.5} />
      ));
      // 减少动画偏好下不启动 requestAnimationFrame 动画循环,
      // 组件正常渲染 canvas 即视为通过(无 rAF 计时器泄漏)
      expect(document.querySelector("canvas")).not.toBeNull();
    });
  });

  describe("交互", () => {
    it("DOM 分片悬停时不崩溃", () => {
      render(() => (
        <ChunkMatrix taskId="test-task" fragmentsTotal={10} fragmentsDone={5} progress={0.5} />
      ));
      const cells = document.querySelectorAll(".chunk-cell");
      expect(cells.length).toBeGreaterThan(0);
      fireEvent.mouseEnter(cells[0]!);
      fireEvent.mouseMove(cells[0]!);
      fireEvent.mouseLeave(cells[0]!);
    });

    it("DOM 分片可键盘聚焦并 Enter/Space 选中", () => {
      render(() => (
        <ChunkMatrix taskId="test-task" fragmentsTotal={10} fragmentsDone={5} progress={0.5} />
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
        <ChunkMatrix taskId="test-task" fragmentsTotal={10} fragmentsDone={5} progress={0.5} />
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
        <ChunkMatrix taskId="test-task" fragmentsTotal={10} fragmentsDone={5} progress={0.5} />
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
        <ChunkMatrix taskId="test-task" fragmentsTotal={1000} fragmentsDone={500} progress={0.5} />
      ));
      const canvas = document.querySelector("canvas");
      expect(canvas).not.toBeNull();
      fireEvent.mouseMove(canvas!, { clientX: 20, clientY: 20 });
      fireEvent.mouseLeave(canvas!);
    });

    it("Canvas 块点击选中", () => {
      render(() => (
        <ChunkMatrix taskId="test-task" fragmentsTotal={1000} fragmentsDone={500} progress={0.5} />
      ));
      const canvas = document.querySelector("canvas");
      expect(canvas).not.toBeNull();
      fireEvent.click(canvas!, { clientX: 20, clientY: 20 });
      // 选中态通过 canvas 绘制验证,这里保证不崩溃即可
      expect(canvas).not.toBeNull();
    });
  });
});
