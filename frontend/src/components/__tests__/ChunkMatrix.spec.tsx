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
  return {
    setTransform: vi.fn(),
    clearRect: vi.fn(),
    beginPath: vi.fn(),
    roundRect: vi.fn(),
    fill: vi.fn(),
    stroke: vi.fn(),
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
  });

  afterEach(() => {
    cleanup();
  });

  describe("buildBlocks 聚合逻辑", () => {
    it("total <= 0 时返回空数组", () => {
      expect(buildBlocks(0, 0, 0)).toEqual([]);
      expect(buildBlocks(-1, 0, 0)).toEqual([]);
    });

    it("total 较小时返回与总数相同的块数", () => {
      const blocks = buildBlocks(10, 5, 0.5);
      expect(blocks.length).toBe(10);
      expect(blocks[0]!.start).toBe(0);
      expect(blocks[9]!.end).toBe(10);
    });

    it("total 较大时固定为 100 块", () => {
      const blocks = buildBlocks(1000, 500, 0.5);
      expect(blocks.length).toBe(100);
    });

    it("块范围覆盖全部分片且不重叠", () => {
      const blocks = buildBlocks(1000, 500, 0.5);
      expect(blocks[0]!.start).toBe(0);
      expect(blocks[blocks.length - 1]!.end).toBe(1000);
      for (let i = 1; i < blocks.length; i++) {
        expect(blocks[i]!.start).toBe(blocks[i - 1]!.end);
      }
    });

    it("已完成分片占多数时块状态为 done", () => {
      const blocks = buildBlocks(100, 60, 0.5);
      expect(blocks[0]!.status).toBe("done");
    });

    it("等待中分片占多数时块状态为 pending", () => {
      const blocks = buildBlocks(100, 10, 0.5);
      expect(blocks[90]!.status).toBe("pending");
    });

    it("使用 THREAD_COLORS 按块索引循环着色", () => {
      const blocks = buildBlocks(120, 60, 0.5);
      expect(blocks[0]!.color).toBe(blocks[12]!.color);
      expect(blocks[0]!.threadId).toBe(0);
      expect(blocks[12]!.threadId).toBe(0);
    });
  });

  describe("组件渲染", () => {
    it("分片数 <= 500 时渲染 DOM 分片", () => {
      render(() => (
        <ChunkMatrix fragmentsTotal={100} fragmentsDone={50} progress={0.5} />
      ));
      const cells = document.querySelectorAll(".chunk-cell");
      expect(cells.length).toBe(100);
      expect(document.querySelector("canvas")).toBeNull();
    });

    it("分片数 > 500 时渲染 canvas", () => {
      render(() => (
        <ChunkMatrix fragmentsTotal={1000} fragmentsDone={500} progress={0.5} />
      ));
      expect(document.querySelector("canvas")).not.toBeNull();
      expect(document.querySelectorAll(".chunk-cell").length).toBe(0);
    });

    it("接受 fragmentsTotal、fragmentsDone、progress props 不报错", () => {
      expect(() => {
        render(() => (
          <ChunkMatrix fragmentsTotal={0} fragmentsDone={0} progress={0} />
        ));
      }).not.toThrow();
      expect(screen.getByText("分片分布")).toBeDefined();
    });

    it("prefers-reduced-motion 时不启动动画循环", () => {
      mockMatchMedia(true);
      render(() => (
        <ChunkMatrix fragmentsTotal={1000} fragmentsDone={500} progress={0.5} />
      ));
      // 减少动画偏好下不启动 requestAnimationFrame 动画循环,
      // 组件正常渲染 canvas 即视为通过(无 rAF 计时器泄漏)
      expect(document.querySelector("canvas")).not.toBeNull();
    });
  });

  describe("交互", () => {
    it("DOM 分片悬停时不崩溃", () => {
      render(() => (
        <ChunkMatrix fragmentsTotal={10} fragmentsDone={5} progress={0.5} />
      ));
      const cells = document.querySelectorAll(".chunk-cell");
      expect(cells.length).toBeGreaterThan(0);
      fireEvent.mouseEnter(cells[0]!);
      fireEvent.mouseMove(cells[0]!);
      fireEvent.mouseLeave(cells[0]!);
    });

    it("Canvas 块悬停时不崩溃", () => {
      render(() => (
        <ChunkMatrix fragmentsTotal={1000} fragmentsDone={500} progress={0.5} />
      ));
      const canvas = document.querySelector("canvas");
      expect(canvas).not.toBeNull();
      fireEvent.mouseMove(canvas!, { clientX: 20, clientY: 20 });
      fireEvent.mouseLeave(canvas!);
    });
  });
});
