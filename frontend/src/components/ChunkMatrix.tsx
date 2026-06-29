import {
  For,
  Show,
  createMemo,
  createSignal,
  onCleanup,
  createEffect,
  onMount,
} from "solid-js";
import { THREAD_COLORS } from "../utils/format";
import { resolveToken } from "../utils/resolveToken";
import { useReducedMotion } from "../hooks/useReducedMotion";
import { tr, type MessageKey } from "../i18n";

interface ChunkMatrixProps {
  fragmentsTotal: number;
  fragmentsDone: number;
  progress: number;
}

const AGGREGATE_THRESHOLD = 200;
const AGGREGATE_BLOCKS = 100;
const MAX_BLOCKS_PER_ROW = 25;
const MIN_BLOCKS_PER_ROW = 8;
const BLOCK_SIZE = 14;
/* 去 AI 味:缝隙 3→2,对齐 FluxDown gap-[1.5px] 紧凑密度 */
const BLOCK_GAP = 2;

interface ChunkData {
  index: number;
  isDone: boolean;
  isDownloading: boolean;
  threadId: number;
  color: string;
}

export interface ChunkBlock {
  index: number;
  start: number;
  end: number;
  done: number;
  total: number;
  status: "done" | "downloading" | "pending";
  color: string;
  threadId: number;
}

export function buildBlocks(
  total: number,
  done: number,
  progress: number,
): ChunkBlock[] {
  if (total <= 0) return [];
  const blockCount = Math.min(total, AGGREGATE_BLOCKS);
  const blocks: ChunkBlock[] = [];
  for (let i = 0; i < blockCount; i++) {
    const start = Math.floor((i * total) / blockCount);
    const end = Math.max(start + 1, Math.floor(((i + 1) * total) / blockCount));
    const blockTotal = end - start;
    let blockDone = 0;
    let blockDownloading = 0;
    for (let f = start; f < end; f++) {
      if (f < done) {
        blockDone++;
      } else if (f === done && progress < 1) {
        blockDownloading++;
      }
    }
    const blockPending = blockTotal - blockDone - blockDownloading;
    let status: ChunkBlock["status"];
    if (blockDone >= blockDownloading && blockDone >= blockPending) {
      status = "done";
    } else if (blockDownloading >= blockPending) {
      status = "downloading";
    } else {
      status = "pending";
    }
    const threadId = i % THREAD_COLORS.length;
    const color = THREAD_COLORS[threadId];
    blocks.push({
      index: i,
      start,
      end,
      done: blockDone,
      total: blockTotal,
      status,
      color: color ?? resolveToken("--color-accent-primary"),
      threadId,
    });
  }
  return blocks;
}

function statusLabelForChunk(chunk: ChunkData): string {
  if (chunk.isDone) return tr("status.label.completed");
  if (chunk.isDownloading) return tr("status.label.downloading");
  return tr("status.label.pending");
}

function statusLabelForBlock(status: ChunkBlock["status"]): string {
  const map: Record<ChunkBlock["status"], MessageKey> = {
    done: "status.label.completed",
    downloading: "status.label.downloading",
    pending: "status.label.pending",
  };
  return tr(map[status]);
}

export default function ChunkMatrix(props: ChunkMatrixProps) {
  const [tooltipIndex, setTooltipIndex] = createSignal<number | null>(null);
  const [cursorPos, setCursorPos] = createSignal({ x: 0, y: 0 });
  // 自适应列数:根据 wrapper 实际宽度计算,避免固定 25 列在窄面板溢出
  const [blocksPerRow, setBlocksPerRow] = createSignal(MAX_BLOCKS_PER_ROW);
  let currentPulsePhase = 0;
  let tooltipTimer: number | null = null;
  let rafId: number | null = null;
  let resizeObs: ResizeObserver | null = null;
  let gridRef: HTMLDivElement | undefined;
  let canvasRef: HTMLCanvasElement | undefined;
  let wrapperRef: HTMLDivElement | undefined;

  const computeBlocksPerRow = (containerWidth: number): number => {
    // 容器内边距 16*2 = 32px;每格占用 BLOCK_SIZE+BLOCK_GAP,最后一格不算 gap
    const available = Math.max(0, containerWidth - 32);
    const perRow = Math.floor((available + BLOCK_GAP) / (BLOCK_SIZE + BLOCK_GAP));
    return Math.max(MIN_BLOCKS_PER_ROW, Math.min(MAX_BLOCKS_PER_ROW, perRow));
  };

  // 检测用户是否偏好减少动画
  const prefersReducedMotion = useReducedMotion();

  const showTooltip = (index: number) => {
    if (tooltipTimer !== null) clearTimeout(tooltipTimer);
    tooltipTimer = window.setTimeout(() => {
      setTooltipIndex(index);
      tooltipTimer = null;
    }, 150);
  };

  const hideTooltip = () => {
    if (tooltipTimer !== null) {
      clearTimeout(tooltipTimer);
      tooltipTimer = null;
    }
    setTooltipIndex(null);
  };

  const handleMouseMove = (e: MouseEvent) => {
    if (!gridRef) return;
    const rect = gridRef.getBoundingClientRect();
    setCursorPos({ x: e.clientX - rect.left, y: e.clientY - rect.top });
  };

  const handleCanvasMouseMove = (e: MouseEvent) => {
    if (!canvasRef) return;
    const rect = canvasRef.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;
    setCursorPos({ x, y });
    const perRow = blocksPerRow();
    const col = Math.floor(x / (BLOCK_SIZE + BLOCK_GAP));
    const row = Math.floor(y / (BLOCK_SIZE + BLOCK_GAP));
    const idx = row * perRow + col;
    const blockList = blocks();
    if (
      col >= 0 &&
      col < perRow &&
      idx >= 0 &&
      idx < blockList.length
    ) {
      setTooltipIndex(idx);
    } else {
      setTooltipIndex(null);
    }
  };

  const handleCanvasMouseLeave = () => {
    hideTooltip();
  };

  onCleanup(() => {
    if (tooltipTimer !== null) clearTimeout(tooltipTimer);
    if (rafId !== null) cancelAnimationFrame(rafId);
    if (resizeObs !== null) resizeObs.disconnect();
  });

  const shouldAggregate = createMemo(
    () => props.fragmentsTotal > AGGREGATE_THRESHOLD,
  );

  const chunks = createMemo(() => {
    if (props.fragmentsTotal > AGGREGATE_THRESHOLD) return [];
    const total = props.fragmentsTotal;
    const done = props.fragmentsDone;
    const progress = props.progress;
    return Array.from({ length: total }, (_, i) => {
      const isDone = i < done;
      const isDownloading = i === done && progress < 1;
      const threadId = i % THREAD_COLORS.length;
      const color = THREAD_COLORS[threadId];
      return {
        index: i,
        isDone,
        isDownloading,
        threadId,
        color: color ?? resolveToken("--color-accent-primary"),
      };
    });
  });

  const blocks = createMemo(() =>
    buildBlocks(props.fragmentsTotal, props.fragmentsDone, props.progress),
  );

  const canvasLayout = createMemo(() => {
    const blockList = blocks();
    const perRow = blocksPerRow();
    const rows = Math.ceil(blockList.length / perRow);
    const width = perRow * (BLOCK_SIZE + BLOCK_GAP) - BLOCK_GAP;
    const height = rows * (BLOCK_SIZE + BLOCK_GAP) - BLOCK_GAP;
    return { rows, width, height };
  });

  const pendingColor = () => resolveToken("--color-bg-tertiary");

  // 缓存上次 Canvas 尺寸,避免每帧重置(width/height 赋值会清空 GPU 上下文)
  let lastCanvasW = 0;
  let lastCanvasH = 0;

  const drawCanvas = (blockList: ChunkBlock[], phase: number) => {
    if (!canvasRef) return;
    const ctx = canvasRef.getContext("2d");
    if (!ctx) return;
    const dpr = window.devicePixelRatio || 1;
    const { width, height } = canvasLayout();
    const w = Math.floor(width * dpr);
    const h = Math.floor(height * dpr);
    // 仅在尺寸变化时重置 Canvas,避免每帧 GPU 上下文重建
    if (w !== lastCanvasW || h !== lastCanvasH) {
      canvasRef.width = w;
      canvasRef.height = h;
      canvasRef.style.width = `${width}px`;
      canvasRef.style.height = `${height}px`;
      lastCanvasW = w;
      lastCanvasH = h;
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, width, height);

    const pulse = Math.sin(phase * Math.PI * 2);
    const pending = pendingColor();
    /* 去 AI 味:圆角 3→2,对齐 DOM 模式 */
    const radius = 2;
    const perRow = blocksPerRow();

    blockList.forEach((block, i) => {
      const row = Math.floor(i / perRow);
      const col = i % perRow;
      const x = col * (BLOCK_SIZE + BLOCK_GAP);
      const y = row * (BLOCK_SIZE + BLOCK_GAP);

      ctx.beginPath();
      ctx.roundRect(x, y, BLOCK_SIZE, BLOCK_SIZE, radius);

      if (block.status === "pending") {
        ctx.fillStyle = pending;
      } else {
        if (block.status === "downloading" && !prefersReducedMotion()) {
          ctx.globalAlpha = 0.55 + 0.45 * pulse;
        }
        ctx.fillStyle = block.color;
      }
      ctx.fill();
      ctx.globalAlpha = 1;

      if (block.status === "done") {
        /* 去 AI 味:完成态阴影 2→1,降低辉光 */
        ctx.shadowColor = `${block.color}66`;
        ctx.shadowBlur = 1;
        ctx.stroke();
        ctx.shadowBlur = 0;
      }
    });
  };

  const startPulse = () => {
    if (rafId !== null) return;
    // 用户偏好减少动画时，仅绘制一次静态帧，不启动动画循环
    if (prefersReducedMotion()) {
      drawCanvas(blocks(), 0);
      return;
    }
    // 下载活跃时降级到 30 FPS(每帧间隔 ≥33ms),释放 CPU/GPU 资源给下载线程
    const MIN_FRAME_MS = 33; // 30 FPS
    let lastFrameTime = 0;
    const loop = (now: number) => {
      // 帧率节流:距上次绘制不足 33ms 时跳过本帧
      if (now - lastFrameTime < MIN_FRAME_MS) {
        rafId = requestAnimationFrame(loop);
        return;
      }
      lastFrameTime = now;
      currentPulsePhase = (now % 1500) / 1500;
      const blockList = blocks();
      const stillDownloading = blockList.some(
        (b) => b.status === "downloading",
      );
      if (shouldAggregate()) {
        drawCanvas(blockList, currentPulsePhase);
      }
      if (stillDownloading) {
        rafId = requestAnimationFrame(loop);
      } else {
        rafId = null;
      }
    };
    rafId = requestAnimationFrame(loop);
  };

  onMount(() => {
    if (wrapperRef) {
      setBlocksPerRow(computeBlocksPerRow(wrapperRef.clientWidth));
      // jsdom 测试环境无 ResizeObserver,守护避免 ReferenceError
      if (typeof ResizeObserver !== "undefined") {
        resizeObs = new ResizeObserver((entries) => {
          for (const entry of entries) {
            setBlocksPerRow(computeBlocksPerRow(entry.contentRect.width));
          }
        });
        resizeObs.observe(wrapperRef);
      }
    }
    if (shouldAggregate()) {
      drawCanvas(blocks(), currentPulsePhase);
    }
  });

  createEffect(() => {
    const blockList = blocks();
    if (shouldAggregate()) {
      drawCanvas(blockList, currentPulsePhase);
    }
  });

  createEffect(() => {
    if (shouldAggregate() && blocks().some((b) => b.status === "downloading")) {
      startPulse();
    }
  });

  const tooltipChunk = createMemo(() => {
    const idx = tooltipIndex();
    if (idx === null || shouldAggregate()) return null;
    const chunkList = chunks();
    const chunk = chunkList[idx];
    if (!chunk) return null;
    return { idx, chunk, total: chunkList.length };
  });

  const tooltipBlock = createMemo(() => {
    const idx = tooltipIndex();
    if (idx === null || !shouldAggregate()) return null;
    const blockList = blocks();
    const block = blockList[idx];
    if (!block) return null;
    return { block };
  });

  const tooltipPos = createMemo(() => {
    const pos = cursorPos();
    return {
      left: Math.min(pos.x + 12, (wrapperRef?.clientWidth || 360) - 160),
      top: pos.y - 60,
    };
  });

  return (
    <div>
      <div class="section-label" style={{ "margin-bottom": "12px" }}>
        {tr("chunk.sectionLabel")}
        <span
          class="chunk-info-hint"
          title={tr("chunk.infoHint")}
        >
          ?
        </span>
      </div>

      <div
        class="glass"
        style={{
          padding: "16px",
          "border-radius": "12px",
          position: "relative",
        }}
        ref={wrapperRef}
        onMouseMove={handleMouseMove}
      >
        <Show
          when={shouldAggregate()}
          fallback={
            <div class="flex flex-wrap" style={{ gap: "2px" }} ref={gridRef}>
              <For each={chunks()}>
                {(chunk) => (
                  <div
                    class="chunk-cell"
                    style={{
                      width: "14px",
                      height: "14px",
                      "border-radius": "2px",
                      background: chunk.isDone
                        ? chunk.color
                        : chunk.isDownloading
                          ? chunk.color
                          : "var(--color-bg-tertiary)",
                      /* 去 AI 味:移除完成格 inset 描边,改纯填充 */
                      "box-shadow": "none",
                      animation: chunk.isDownloading
                        ? `chunk-appear 200ms cubic-bezier(0.34, 1.56, 0.64, 1) forwards, chunk-pulse 1.5s ease-in-out infinite`
                        : `chunk-appear 200ms cubic-bezier(0.34, 1.56, 0.64, 1) forwards`,
                      "animation-delay": chunk.isDownloading
                        ? `${chunk.index * 5}ms, ${(chunk.index % blocksPerRow()) * 0.05 + Math.floor(chunk.index / blocksPerRow()) * 0.1}s`
                        : `${chunk.index * 5}ms`,
                      opacity: 0,
                    }}
                    onMouseEnter={() => showTooltip(chunk.index)}
                    onMouseLeave={() => hideTooltip()}
                  />
                )}
              </For>
            </div>
          }
        >
          <canvas
            ref={canvasRef}
            style={{ display: "block" }}
            onMouseMove={handleCanvasMouseMove}
            onMouseLeave={handleCanvasMouseLeave}
          />
        </Show>

        {/* Chunk tooltip */}
        <Show when={tooltipChunk()} keyed>
          {(tooltip) => (
            <div
              class="chunk-tooltip"
              style={{
                left: `${tooltipPos().left}px`,
                top: `${tooltipPos().top}px`,
              }}
            >
              <div
                class="chunk-tooltip-dot"
                style={{
                  background:
                    tooltip.chunk.isDone || tooltip.chunk.isDownloading
                      ? tooltip.chunk.color
                      : "var(--color-bg-tertiary)",
                }}
              />
              <div class="chunk-tooltip-body">
                <div class="chunk-tooltip-title">
                  {tr("chunk.tooltip.fragment")} #{tooltip.idx + 1}
                  <span class="chunk-tooltip-subtitle">/ {tooltip.total}</span>
                </div>
                <div class="chunk-tooltip-meta">
                  <span
                    style={{
                      color: tooltip.chunk.isDone
                        ? "var(--color-status-completed)"
                        : tooltip.chunk.isDownloading
                          ? "var(--color-status-downloading)"
                          : "var(--color-text-tertiary)",
                    }}
                  >
                    {statusLabelForChunk(tooltip.chunk)}
                  </span>
                </div>
              </div>
            </div>
          )}
        </Show>

        {/* Block tooltip */}
        <Show when={tooltipBlock()} keyed>
          {(tooltip) => (
            <div
              class="chunk-tooltip"
              style={{
                left: `${tooltipPos().left}px`,
                top: `${tooltipPos().top}px`,
              }}
            >
              <div
                class="chunk-tooltip-dot"
                style={{
                  background:
                    tooltip.block.status === "pending"
                      ? "var(--color-bg-tertiary)"
                      : tooltip.block.color,
                }}
              />
              <div class="chunk-tooltip-body">
                <div class="chunk-tooltip-title">
                  {tr("chunk.tooltip.fragment")} #{tooltip.block.start + 1}-#{tooltip.block.end}
                </div>
                <div class="chunk-tooltip-meta">
                  <span
                    style={{
                      color:
                        tooltip.block.status === "done"
                          ? "var(--color-status-completed)"
                          : tooltip.block.status === "downloading"
                            ? "var(--color-status-downloading)"
                            : "var(--color-text-tertiary)",
                    }}
                  >
                    {statusLabelForBlock(tooltip.block.status)}
                  </span>
                </div>
                <div class="chunk-tooltip-row" style={{ "margin-top": "4px" }}>
                  <div class="chunk-tooltip-label">{tr("chunk.tooltip.completed")}</div>
                  <div class="chunk-tooltip-value">
                    {tooltip.block.done} / {tooltip.block.total}
                  </div>
                </div>
              </div>
            </div>
          )}
        </Show>

        {/* Legend */}
        <div class="flex items-center gap-4" style={{ "margin-top": "12px" }}>
          <LegendItem color="var(--color-status-completed)" label={tr("status.label.completed")} />
          <LegendItem color="var(--color-bg-tertiary)" label={tr("status.label.notStarted")} />
          <LegendItem
            color="var(--color-status-downloading)"
            label={tr("status.label.downloading")}
            pulse
          />
          <LegendItem color="var(--color-status-error)" label={tr("status.label.error")} />
        </div>
      </div>
    </div>
  );
}

function LegendItem(props: { color: string; label: string; pulse?: boolean }) {
  return (
    <div class="flex items-center gap-1.5">
      <div
        style={{
          width: "8px",
          height: "8px",
          "border-radius": "2px",
          background: props.color,
          animation: props.pulse
            ? "chunk-pulse 1.5s ease-in-out infinite"
            : "none",
        }}
      />
      <span
        style={{ "font-size": "11px", color: "var(--color-text-tertiary)" }}
      >
        {props.label}
      </span>
    </div>
  );
}
