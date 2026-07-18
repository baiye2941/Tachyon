import {
  Index,
  Show,
  createMemo,
  createSignal,
  onCleanup,
  createEffect,
  onMount,
} from "solid-js";
import { resolveToken } from "../utils/resolveToken";
import { useReducedMotion } from "../hooks/useReducedMotion";
import { tr, type MessageKey } from "../i18n";
import {
  getTaskFragmentData,
  mergeFragmentDelta,
  type TaskFragmentData,
} from "../stores/taskFragments";
import FragmentFill from "./FragmentFill";

interface ChunkMatrixProps {
  taskId: string;
  fragmentsTotal: number;
  fragmentsDone: number;
  progress: number;
  /** 文件总字节数;用于把分片字节进度换算为充能比例,未知时充能条不渲染 */
  fileSize?: number | null;
}

const AGGREGATE_THRESHOLD = 200;
const AGGREGATE_BLOCKS = 100;
const EMPTY_SET = new Set<number>();
const EMPTY_BYTES_MAP = new Map<number, number>();
const MAX_BLOCKS_PER_ROW = 25;
const MIN_BLOCKS_PER_ROW = 8;
const BLOCK_SIZE = 14;
/* 缝隙 2px,对齐 DOM 与 Canvas 的紧凑密度 */
const BLOCK_GAP = 2;

/** 比较两个数字集合的内容是否一致,用于稳定 memo 引用。 */
function setsEqual(a: Set<number>, b: Set<number>): boolean {
  if (a.size !== b.size) return false;
  for (const x of a) {
    if (!b.has(x)) return false;
  }
  return true;
}

/** 比较两个字节进度快照的内容是否一致,用于稳定 memo 引用。 */
function bytesMapEqual(a: Map<number, number>, b: Map<number, number>): boolean {
  if (a.size !== b.size) return false;
  for (const [k, v] of a) {
    if (b.get(k) !== v) return false;
  }
  return true;
}

/** 分片状态色:仅与下载状态绑定。 */
const STATUS_COLOR_VARS: Record<"done" | "downloading" | "pending", string> = {
  done: "var(--color-status-completed)",
  downloading: "var(--color-status-downloading)",
  pending: "var(--color-status-pending)",
};

/** Canvas 无法解析 CSS 变量,用同名 token 解析为当前主题的具体颜色。 */
const STATUS_TOKENS: Record<"done" | "downloading" | "pending", string> = {
  done: "--color-status-completed",
  downloading: "--color-status-downloading",
  pending: "--color-status-pending",
};

export interface ChunkBlock {
  index: number;
  start: number;
  end: number;
  done: number;
  total: number;
  status: "done" | "downloading" | "pending";
  color: string;
}

export function buildBlocks(
  total: number,
  doneSet: Set<number>,
  downloadingSet: Set<number>,
): ChunkBlock[] {
  if (total <= 0) return [];
  const blockCount = Math.min(total, AGGREGATE_BLOCKS);
  // 真实活跃分片由 downloadingSet 决定(后端 Started 事件驱动),
  // 不再依赖 maxDoneIdx + band 位置启发式。
  // 优化:不再扫描全量分片区间,而是直接遍历 doneSet/downloadingSet,
  // 时间复杂度 O(blockCount + |doneSet| + |downloadingSet|)。
  const blocks: ChunkBlock[] = [];
  for (let i = 0; i < blockCount; i++) {
    const start = Math.floor((i * total) / blockCount);
    const end = Math.max(start + 1, Math.floor(((i + 1) * total) / blockCount));
    blocks.push({
      index: i,
      start,
      end,
      done: 0,
      total: end - start,
      status: "pending",
      color: STATUS_COLOR_VARS.pending,
    });
  }

  for (const idx of doneSet) {
    if (idx < 0 || idx >= total) continue;
    const blockIdx = Math.floor((idx * blockCount) / total);
    if (blockIdx < blockCount) {
      blocks[blockIdx]!.done++;
    }
  }

  const downloadingCounts = new Array(blockCount).fill(0) as number[];
  for (const idx of downloadingSet) {
    if (idx < 0 || idx >= total || doneSet.has(idx)) continue;
    const blockIdx = Math.floor((idx * blockCount) / total);
    if (blockIdx < blockCount) {
      downloadingCounts[blockIdx] = (downloadingCounts[blockIdx] ?? 0) + 1;
    }
  }

  for (let i = 0; i < blockCount; i++) {
    const block = blocks[i]!;
    const downloading = downloadingCounts[i]!;
    const pending = block.total - block.done - downloading;
    let status: ChunkBlock["status"];
    if (block.done >= downloading && block.done >= pending) {
      status = "done";
    } else if (downloading >= pending) {
      status = "downloading";
    } else {
      status = "pending";
    }
    block.status = status;
    block.color = STATUS_COLOR_VARS[status];
  }

  return blocks;
}

/**
 * 计算每个聚合 block 的平均字节进度(仅统计 bytesMap 中的活跃分片)。
 *
 * 分母用整片预估大小(fileSize/total)× block 内活跃分片数,结果 clamp 到 [0,1];
 * fileSize 未知或参数非法时返回全 0(诚实不画深度,与 DOM 端充能条行为一致)。
 * blockIdx 映射公式与 buildBlocks 保持一致。
 * 时间复杂度 O(|bytesMap| + blockCount),bytesMap 仅含活跃分片(上限为并发分片数)。
 */
export function buildBlockProgress(
  total: number,
  blockCount: number,
  bytesMap: Map<number, number>,
  fileSize: number | null | undefined,
): number[] {
  const size = Math.max(0, blockCount);
  const progress = new Array<number>(size).fill(0);
  if (!fileSize || fileSize <= 0 || total <= 0 || size === 0) return progress;
  const perFragSize = fileSize / total;
  const totalBytes = new Array<number>(size).fill(0);
  const counts = new Array<number>(size).fill(0);
  for (const [idx, downloaded] of bytesMap) {
    if (idx < 0 || idx >= total) continue;
    const blockIdx = Math.floor((idx * size) / total);
    totalBytes[blockIdx]! += downloaded;
    counts[blockIdx]! += 1;
  }
  for (let i = 0; i < size; i++) {
    if (counts[i]! > 0) {
      const blockExpected = perFragSize * counts[i]!;
      progress[i] = Math.min(1, totalBytes[i]! / blockExpected);
    }
  }
  return progress;
}

/**
 * 把 resolveToken 解析出的 #rrggbb 颜色转为带透明度的 rgba() 字符串。
 * 非 hex 输入(异常主题值)原样返回,保证 addColorStop 不抛错。
 */
function withAlpha(color: string, alpha: number): string {
  const match = /^#([0-9a-f]{6})$/i.exec(color.trim());
  if (!match) return color;
  const value = parseInt(match[1]!, 16);
  const r = (value >> 16) & 255;
  const g = (value >> 8) & 255;
  const b = value & 255;
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

function statusLabelForChunk(status: ChunkBlock["status"]): string {
  const map: Record<ChunkBlock["status"], MessageKey> = {
    done: "status.label.completed",
    downloading: "status.label.downloading",
    pending: "status.label.pending",
  };
  return tr(map[status]);
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
  const [hoverIndex, setHoverIndex] = createSignal<number | null>(null);
  const [selectedIndex, setSelectedIndex] = createSignal<number | null>(null);
  // 自适应列数:根据 wrapper 实际宽度计算,避免固定 25 列在窄面板溢出
  const [blocksPerRow, setBlocksPerRow] = createSignal(MAX_BLOCKS_PER_ROW);
  let currentPulsePhase = 0;
  let rafId: number | null = null;
  let resizeObs: ResizeObserver | null = null;
  let gridRef: HTMLDivElement | undefined;
  let canvasRef: HTMLCanvasElement | undefined;
  let wrapperRef: HTMLDivElement | undefined;

  const computeBlocksPerRow = (containerWidth: number): number => {
    // 容器内边距 16*2 = 32px;每格占用 BLOCK_SIZE+BLOCK_GAP,最后一格不算 gap
    const available = Math.max(0, containerWidth - 32);
    const perRow = Math.floor(
      (available + BLOCK_GAP) / (BLOCK_SIZE + BLOCK_GAP),
    );
    return Math.max(MIN_BLOCKS_PER_ROW, Math.min(MAX_BLOCKS_PER_ROW, perRow));
  };

  // 检测用户是否偏好减少动画
  const prefersReducedMotion = useReducedMotion();

  let hideHoverTimer: number | null = null;

  const showHover = (index: number) => {
    if (hideHoverTimer !== null) {
      clearTimeout(hideHoverTimer);
      hideHoverTimer = null;
    }
    setHoverIndex(index);
  };

  const hideHover = () => {
    if (hideHoverTimer !== null) return;
    hideHoverTimer = window.setTimeout(() => {
      setHoverIndex(null);
      hideHoverTimer = null;
    }, 120);
  };

  const clearSelection = () => setSelectedIndex(null);

  // 在 grid 上统一计算当前 hover 的 cell,避免 cell 间 mouseleave 导致 tooltip 闪烁
  const handleGridMouseMove = (e: MouseEvent) => {
    if (!gridRef) return;
    const rect = gridRef.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;
    const perRow = blocksPerRow();
    const col = Math.floor(x / (BLOCK_SIZE + BLOCK_GAP));
    const row = Math.floor(y / (BLOCK_SIZE + BLOCK_GAP));
    const idx = row * perRow + col;
    if (
      col >= 0 &&
      col < perRow &&
      idx >= 0 &&
      idx < props.fragmentsTotal
    ) {
      setHoverIndex(idx);
    } else {
      hideHover();
    }
  };

  const handleGridMouseLeave = () => {
    hideHover();
  };

  const handleCanvasMouseMove = (e: MouseEvent) => {
    if (!canvasRef) return;
    const rect = canvasRef.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;
    const perRow = blocksPerRow();
    const col = Math.floor(x / (BLOCK_SIZE + BLOCK_GAP));
    const row = Math.floor(y / (BLOCK_SIZE + BLOCK_GAP));
    const idx = row * perRow + col;
    const blockList = blocks();
    if (col >= 0 && col < perRow && idx >= 0 && idx < blockList.length) {
      setHoverIndex(idx);
    } else {
      hideHover();
    }
  };

  const handleCanvasMouseLeave = () => {
    hideHover();
  };

  const handleCanvasClick = (e: MouseEvent) => {
    if (!canvasRef) return;
    const rect = canvasRef.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;
    const perRow = blocksPerRow();
    const col = Math.floor(x / (BLOCK_SIZE + BLOCK_GAP));
    const row = Math.floor(y / (BLOCK_SIZE + BLOCK_GAP));
    const idx = row * perRow + col;
    const blockList = blocks();
    if (col >= 0 && col < perRow && idx >= 0 && idx < blockList.length) {
      setSelectedIndex((prev) => (prev === idx ? null : idx));
    } else {
      clearSelection();
    }
  };

  const handleKeyDown = (e: KeyboardEvent, index: number) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      setSelectedIndex((prev) => (prev === index ? null : index));
    } else if (e.key === "Escape") {
      clearSelection();
    }
  };

  onCleanup(() => {
    if (rafId !== null) cancelAnimationFrame(rafId);
    if (resizeObs !== null) resizeObs.disconnect();
    if (hideHoverTimer !== null) clearTimeout(hideHoverTimer);
  });

  const shouldAggregate = createMemo(
    () => props.fragmentsTotal > AGGREGATE_THRESHOLD,
  );

  const rawFragData = createMemo(() => getTaskFragmentData(props.taskId));

  /**
   * 稳定分片数据引用。
   *
   * taskFragments store 每次 merge delta 都会产生新的 TaskFragmentData 对象与新的 Set,
   * 但多数 progress tick 只是 fragmentsDone/fragmentsTotal 等数字变化,分片集合内容并未变。
   * 通过内容比对返回相同引用,避免下游 memo 与 DOM 单元格随每次 tick 全量重建。
   *
   * bytesMap 必须参与比对:下载中的 tick 往往只有字节数变化(集合不变),
   * 漏比会让 FragmentFill 充能条与 tooltip 百分比冻结在首个快照。
   */
  let lastFragData: TaskFragmentData | undefined;
  const fragData = createMemo(() => {
    const current = rawFragData();
    if (!current) {
      lastFragData = undefined;
      return undefined;
    }
    if (
      lastFragData &&
      lastFragData.total === current.total &&
      lastFragData.finalized === current.finalized &&
      setsEqual(lastFragData.doneSet, current.doneSet) &&
      setsEqual(lastFragData.downloadingSet, current.downloadingSet) &&
      bytesMapEqual(lastFragData.bytesMap, current.bytesMap)
    ) {
      return lastFragData;
    }
    lastFragData = current;
    return current;
  });

  /** DOM 模式下仅缓存稳定的分片索引数组,不在每次 tick 重建对象。 */
  const fragmentIndices = createMemo(() => {
    if (shouldAggregate()) return [];
    return Array.from({ length: props.fragmentsTotal }, (_, i) => i);
  });

  const blocks = createMemo(() => {
    const data = fragData();
    const doneSet = data?.doneSet ?? EMPTY_SET;
    const downloadingSet = data?.downloadingSet ?? EMPTY_SET;
    return buildBlocks(props.fragmentsTotal, doneSet, downloadingSet);
  });

  /**
   * 每个聚合 block 的平均字节进度(仅 downloading 活跃分片),Canvas 渐变深度填充用。
   * bytesMap 随 250ms 快照更新;重算代价 O(|活跃分片| + blockCount),与 buildBlocks 同级。
   */
  const blockProgress = createMemo(() => {
    const data = fragData();
    const bytesMap = data?.bytesMap ?? EMPTY_BYTES_MAP;
    return buildBlockProgress(
      props.fragmentsTotal,
      blocks().length,
      bytesMap,
      props.fileSize,
    );
  });

  // 整块下载兜底:任务完成但 doneSet 为空(单分片整块下载无 Chunk::completed 事件)
  createEffect(() => {
    const data = fragData();
    if (props.progress >= 1 && data && data.doneSet.size === 0) {
      mergeFragmentDelta(props.taskId, [0], []);
    }
  });

  const canvasLayout = createMemo(() => {
    const blockList = blocks();
    const perRow = blocksPerRow();
    const rows = Math.ceil(blockList.length / perRow);
    const width = perRow * (BLOCK_SIZE + BLOCK_GAP) - BLOCK_GAP;
    const height = rows * (BLOCK_SIZE + BLOCK_GAP) - BLOCK_GAP;
    return { rows, width, height };
  });

  /** 缓存 Canvas 颜色解析,避免每帧重复读取 CSS 变量。 */
  const getStatusFill = (() => {
    const cache = new Map<ChunkBlock["status"], string>();
    return (status: ChunkBlock["status"]): string => {
      let color = cache.get(status);
      if (color === undefined) {
        color = resolveToken(STATUS_TOKENS[status]);
        cache.set(status, color);
      }
      return color;
    };
  })();

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
    const radius = 5;
    const perRow = blocksPerRow();
    const selected = selectedIndex();
    const progressByBlock = blockProgress();
    const hasMotion = !prefersReducedMotion();
    const accentColor = resolveToken("--color-accent-primary");
    const sweepPhase = hasMotion ? (phase * 2) % 1 : 0;
    const topHighlightRadii: [number, number, number, number] = [
      Math.max(1, radius - 1),
      Math.max(1, radius - 1),
      1,
      1,
    ];

    // 单次遍历:每个块依次绘制 7 个效果,减少循环次数与 save/restore
    for (let i = 0; i < blockList.length; i++) {
      const block = blockList[i]!;
      const { x, y } = blockCoords(i, perRow);
      const fillColor = getStatusFill(block.status);

      // 1) 下载中块外发光
      if (block.status === "downloading" && hasMotion) {
        ctx.shadowBlur = 8 + 4 * pulse;
        ctx.shadowColor = fillColor;
        ctx.globalAlpha = 0.18 + 0.14 * pulse;
        ctx.fillStyle = fillColor;
        ctx.beginPath();
        ctx.roundRect(x, y, BLOCK_SIZE, BLOCK_SIZE, radius);
        ctx.fill();
        ctx.shadowBlur = 0;
      }

      // 2) 块底色
      ctx.globalAlpha =
        block.status === "downloading" && hasMotion
          ? 0.85 + 0.15 * pulse
          : 1;
      ctx.fillStyle = fillColor;
      ctx.beginPath();
      ctx.roundRect(x, y, BLOCK_SIZE, BLOCK_SIZE, radius);
      ctx.fill();
      ctx.globalAlpha = 1;

      // 下载中块渐变深度:宽度 = BLOCK_SIZE × 块平均字节进度,
      // 颜色取 downloading token 解析色的两档低透明度,与 DOM 端充能条同语义;
      // 非动画效果,reduced-motion 下同样绘制。
      if (block.status === "downloading") {
        const depth = progressByBlock[i] ?? 0;
        if (depth > 0) {
          const fillW = BLOCK_SIZE * depth;
          const grad = ctx.createLinearGradient(x, y, x + fillW, y);
          grad.addColorStop(0, withAlpha(fillColor, 0.25));
          grad.addColorStop(1, withAlpha(fillColor, 0.55));
          ctx.fillStyle = grad;
          ctx.beginPath();
          ctx.roundRect(x, y, fillW, BLOCK_SIZE, radius);
          ctx.fill();
        }
      }

      // 3) 完成态内发光 + 顶部高光
      if (block.status === "done") {
        ctx.shadowColor = fillColor;
        ctx.shadowBlur = 5;
        ctx.strokeStyle = fillColor;
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.roundRect(x, y, BLOCK_SIZE, BLOCK_SIZE, radius);
        ctx.stroke();
        ctx.shadowBlur = 0;

        ctx.fillStyle = "rgba(255,255,255,0.12)";
        ctx.beginPath();
        ctx.roundRect(
          x + 1,
          y + 1,
          BLOCK_SIZE - 2,
          BLOCK_SIZE * 0.45,
          topHighlightRadii,
        );
        ctx.fill();
      }

      // 4) 等待态点阵纹理
      if (block.status === "pending") {
        ctx.fillStyle = "rgba(255,255,255,0.05)";
        for (let px = x + 2; px < x + BLOCK_SIZE; px += 4) {
          for (let py = y + 2; py < y + BLOCK_SIZE; py += 4) {
            ctx.fillRect(px - 0.6, py - 0.6, 1.2, 1.2);
          }
        }
      }

      // 5) 统一顶部高光 + 底部暗边
      ctx.fillStyle = "rgba(255,255,255,0.08)";
      ctx.fillRect(x, y, BLOCK_SIZE, 1);
      ctx.fillStyle = "rgba(0,0,0,0.35)";
      ctx.fillRect(x, y + BLOCK_SIZE - 1, BLOCK_SIZE, 1);

      // 6) 下载中扫描光带
      if (block.status === "downloading" && hasMotion) {
        const sweepX = x - 18 + sweepPhase * (BLOCK_SIZE + 18);
        const grad = ctx.createLinearGradient(sweepX, y, sweepX + 14, y);
        grad.addColorStop(0, "rgba(255,255,255,0)");
        grad.addColorStop(0.5, "rgba(255,255,255,0.45)");
        grad.addColorStop(1, "rgba(255,255,255,0)");
        ctx.save();
        ctx.beginPath();
        ctx.roundRect(
          x + 1,
          y + 1,
          BLOCK_SIZE - 2,
          BLOCK_SIZE - 2,
          radius - 1,
        );
        ctx.clip();
        ctx.fillStyle = grad;
        ctx.fillRect(sweepX, y, 14, BLOCK_SIZE);
        ctx.restore();
      }
    }

    // 7) 选中态外环(在单次遍历后绘制,确保阴影覆盖相邻块)
    if (selected !== null) {
      const { x, y } = blockCoords(selected, perRow);
      ctx.save();
      ctx.shadowColor = accentColor;
      ctx.shadowBlur = 6;
      ctx.strokeStyle = accentColor;
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.roundRect(
        x - 2,
        y - 2,
        BLOCK_SIZE + 4,
        BLOCK_SIZE + 4,
        radius + 1,
      );
      ctx.stroke();
      ctx.restore();
    }
  };

  const blockCoords = (index: number, perRow: number) => {
    const row = Math.floor(index / perRow);
    const col = index % perRow;
    return {
      x: col * (BLOCK_SIZE + BLOCK_GAP),
      y: row * (BLOCK_SIZE + BLOCK_GAP),
    };
  };

  const startPulse = () => {
    if (rafId !== null) return;
    if (prefersReducedMotion()) {
      drawCanvas(blocks(), 0);
      return;
    }
    const MIN_FRAME_MS = 33; // 30 FPS
    let lastFrameTime = 0;
    const loop = (now: number) => {
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

  // 点击 wrapper 空白处取消选中
  const handleWrapperClick = (e: MouseEvent) => {
    if (e.target === wrapperRef || e.target === gridRef) {
      clearSelection();
    }
  };

  const handleWrapperKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      clearSelection();
    }
  };

  const tooltipData = createMemo(() => {
    const idx = hoverIndex() ?? selectedIndex();
    if (idx === null) return null;
    if (shouldAggregate()) {
      const blockList = blocks();
      const block = blockList[idx];
      if (!block) return null;
      const percent =
        block.total > 0 ? Math.round((block.done / block.total) * 100) : 0;
      return {
        type: "block" as const,
        idx,
        color: block.color,
        title: tr("chunk.tooltip.fragmentRange", {
          start: block.start + 1,
          end: block.end,
        }),
        statusLabel: statusLabelForBlock(block.status),
        statusClass:
          block.status === "done"
            ? "text-status-completed"
            : block.status === "downloading"
              ? "text-status-downloading"
              : "text-text-tertiary",
        percentText: `${percent}%`,
        detailLabel: tr("chunk.tooltip.completed"),
        detailValue: `${block.done} / ${block.total}`,
      };
    }
    const data = fragData();
    const doneSet = data?.doneSet ?? EMPTY_SET;
    const downloadingSet = data?.downloadingSet ?? EMPTY_SET;
    const isDone = doneSet.has(idx);
    const isDownloading = !isDone && downloadingSet.has(idx);
    const status: ChunkBlock["status"] = isDone
      ? "done"
      : isDownloading
        ? "downloading"
        : "pending";
    // 真实字节进度:downloading 分片按 bytesMap / 整片预估值换算,
    // 替代原先写死的 50%;fileSize 未知时诚实显示 0。
    const downloaded = isDownloading ? (data?.bytesMap.get(idx) ?? 0) : 0;
    const fragmentSize =
      props.fileSize && props.fragmentsTotal > 0
        ? props.fileSize / props.fragmentsTotal
        : 0;
    const percent = isDone
      ? 100
      : isDownloading && fragmentSize > 0
        ? Math.min(100, Math.round((downloaded / fragmentSize) * 100))
        : 0;
    return {
      type: "chunk" as const,
      idx,
      color: STATUS_COLOR_VARS[status],
      title: tr("chunk.tooltip.fragment", { index: idx + 1 }),
      total: props.fragmentsTotal,
      statusLabel: statusLabelForChunk(status),
      statusClass:
        status === "done"
          ? "text-status-completed"
          : status === "downloading"
            ? "text-status-downloading"
            : "text-text-tertiary",
      percentText: `${percent}%`,
    };
  });

  const tooltipVisible = createMemo(
    () =>
      tooltipData() !== null &&
      (hoverIndex() !== null || selectedIndex() !== null),
  );

  // tooltip 现在作为 header 的一部分普通流布局,无需绝对定位计算

  return (
    <div>
      <div
        class="glass chunk-matrix-wrapper"
        ref={wrapperRef}
        onClick={handleWrapperClick}
        onKeyDown={handleWrapperKeyDown}
        role="group"
        aria-label={tr("chunk.sectionLabel")}
      >
        {/* Header: 标题 + tooltip 同行,不遮挡矩阵 */}
        <div class="chunk-matrix-header">
          <div class="chunk-matrix-header-left">
            <span class="section-label">{tr("chunk.sectionLabel")}</span>
            <span class="chunk-info-hint" title={tr("chunk.infoHint")}>
              ?
            </span>
          </div>

          <div
            class="chunk-tooltip"
            classList={{ "chunk-tooltip--visible": tooltipVisible() }}
          >
            <Show when={tooltipData()} keyed>
              {(tip) => (
                <>
                  <div
                    class="chunk-tooltip-dot"
                    style={{ background: tip.color }}
                  />
                  <div class="chunk-tooltip-body">
                    <div class="chunk-tooltip-title">
                      {tip.title}
                      <Show when={tip.type === "chunk"}>
                        <span class="chunk-tooltip-subtitle">
                          / {(tip as { total: number }).total}
                        </span>
                      </Show>
                    </div>
                    <div class="chunk-tooltip-meta">
                      <div class="chunk-tooltip-meta-left">
                        <span class={tip.statusClass}>{tip.statusLabel}</span>
                        <span class="chunk-tooltip-sep">·</span>
                      </div>
                      <span class="chunk-tooltip-value">{tip.percentText}</span>
                    </div>
                    <Show when={tip.type === "block"}>
                      <div class="chunk-tooltip-row">
                        <div class="chunk-tooltip-label">
                          {(tip as { detailLabel: string }).detailLabel}
                        </div>
                        <div class="chunk-tooltip-value">
                          {(tip as { detailValue: string }).detailValue}
                        </div>
                      </div>
                    </Show>
                  </div>
                </>
              )}
            </Show>
          </div>
        </div>

        <Show
          when={shouldAggregate()}
          fallback={
            <div
              class="chunk-grid"
              style={{ gap: `${BLOCK_GAP}px` }}
              ref={gridRef}
              onMouseMove={handleGridMouseMove}
              onMouseLeave={handleGridMouseLeave}
            >
              <Index each={fragmentIndices()}>
                {(idx) => {
                  const status = createMemo(() => {
                    const data = fragData();
                    const doneSet = data?.doneSet ?? EMPTY_SET;
                    const downloadingSet = data?.downloadingSet ?? EMPTY_SET;
                    const index = idx();
                    if (doneSet.has(index)) return "done";
                    if (downloadingSet.has(index)) return "downloading";
                    return "pending";
                  });
                  // 该分片的字节进度比例(仅 downloading 状态有值)
                  const fillProgress = createMemo(() => {
                    const data = fragData();
                    if (!data) return 0;
                    const index = idx();
                    if (!data.downloadingSet.has(index)) return 0;
                    const downloaded = data.bytesMap.get(index) ?? 0;
                    if (downloaded === 0) return 0;
                    // 用整片预估大小作分母(分片大小可能不均,clamp 到 1;
                    // 下载中渐增有活感)
                    const total =
                      props.fileSize && props.fragmentsTotal > 0
                        ? props.fileSize / props.fragmentsTotal
                        : 0;
                    if (total <= 0) return 0;
                    return Math.min(1, downloaded / total);
                  });
                  const isSelected = createMemo(
                    () => selectedIndex() === idx(),
                  );
                  return (
                    <div
                      class="chunk-cell"
                      classList={{
                        "chunk-cell--done": status() === "done",
                        "chunk-cell--downloading":
                          status() === "downloading",
                        "chunk-cell--pending": status() === "pending",
                        "chunk-cell--selected": isSelected(),
                        "chunk-cell--reduced": prefersReducedMotion(),
                      }}
                      data-status={status()}
                      data-index={idx()}
                      role="button"
                      tabIndex={0}
                      aria-label={tr("chunk.tooltip.fragment", {
                        index: idx() + 1,
                      })}
                      onFocus={() => showHover(idx())}
                      onBlur={() => hideHover()}
                      onClick={() =>
                        setSelectedIndex((prev) =>
                          prev === idx() ? null : idx(),
                        )
                      }
                      onKeyDown={(e) => handleKeyDown(e, idx())}
                    >
                      <Show
                        when={
                          status() === "downloading" && fillProgress() > 0
                        }
                      >
                        <FragmentFill
                          progress={fillProgress()}
                          reducedMotion={prefersReducedMotion()}
                        />
                      </Show>
                    </div>
                  );
                }}
              </Index>
            </div>
          }
        >
          <canvas
            class="chunk-canvas"
            ref={canvasRef}
            onMouseMove={handleCanvasMouseMove}
            onMouseLeave={handleCanvasMouseLeave}
            onClick={handleCanvasClick}
            aria-label={tr("chunk.sectionLabel")}
          />
        </Show>

        {/* Legend */}
        <div class="chunk-legend">
          <LegendItem
            color="var(--color-status-completed)"
            label={tr("status.label.completed")}
          />
          <LegendItem
            color="var(--color-status-pending)"
            label={tr("status.label.pending")}
          />
          <LegendItem
            color="var(--color-status-downloading)"
            label={tr("status.label.downloading")}
            pulse
          />
        </div>
      </div>
    </div>
  );
}

function LegendItem(props: { color: string; label: string; pulse?: boolean }) {
  return (
    <div class="chunk-legend-item">
      <div
        class="chunk-legend-dot"
        classList={{ "chunk-legend-dot--pulse": props.pulse }}
        style={{ background: props.color }}
      />
      <span class="chunk-legend-label">{props.label}</span>
    </div>
  );
}
