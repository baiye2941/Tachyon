import {
  For,
  Show,
  createSignal,
  createMemo,
  createEffect,
  onMount,
  onCleanup,
} from "solid-js";
import type { TaskInfo, ListDensity } from "../types";
import TaskItem from "./TaskItem";
import { COLUMNS } from "./taskColumns";
import { $taskSort, toggleSort, sortTasks } from "../stores/taskSort";
import { PlusIcon } from "./icons";
import Button from "../shared/ui/Button";
import { useI18n } from "../i18n";

/** Fixed row heights per density mode (px)
 * spec 8.1:compact 52px / comfortable 72px */
const ITEM_HEIGHTS: Record<ListDensity, number> = {
  comfortable: 72,
  compact: 52,
};

/** Number of off-screen buffer items rendered above/below the viewport */
const BUFFER_COUNT = 3;

interface TaskListProps {
  tasks: TaskInfo[];
  selectedTaskId: string | null;
  onTaskClick: (taskId: string) => void;
  onTaskContextMenu?: (e: MouseEvent, taskId: string) => void;
  isMultiSelectMode: boolean;
  selectedTaskIds: Set<string>;
  density: ListDensity;
  searchQuery?: string;
  onNewTask?: () => void;
}

export default function TaskList(props: TaskListProps) {
  const i18n = useI18n();

  let scrollContainerRef: HTMLDivElement | undefined;
  let rafId: number | null = null;

  // ── Virtual-scroll reactive state ──────────────────────────────
  const [scrollTop, setScrollTop] = createSignal(0);
  const [containerHeight, setContainerHeight] = createSignal(500);

  const itemHeight = createMemo(() => ITEM_HEIGHTS[props.density]);

  // ── 排序(Iteration 07 DI-3):作用于已筛选列表,统一数据源 ──
  const sortedTasks = createMemo(() =>
    sortTasks(props.tasks, $taskSort.state()),
  );

  const totalHeight = createMemo(() => sortedTasks().length * itemHeight());

  /** How many items fit in the visible viewport */
  const visibleCount = createMemo(
    () => Math.ceil(containerHeight() / itemHeight()) + 1,
  );

  /** First index in the render window (including buffer) */
  const startIndex = createMemo(() => {
    const raw = Math.floor(scrollTop() / itemHeight()) - BUFFER_COUNT;
    return Math.max(0, raw);
  });

  /** Last index (exclusive) in the render window (including buffer) */
  const endIndex = createMemo(() => {
    const raw =
      Math.floor(scrollTop() / itemHeight()) + visibleCount() + BUFFER_COUNT;
    return Math.min(sortedTasks().length, raw);
  });

  /** Y-offset for the inner positioning container */
  const offsetY = createMemo(() => startIndex() * itemHeight());

  /** The subset of tasks currently rendered (<For> reconciles by identity) */
  const visibleTasks = createMemo(() =>
    sortedTasks().slice(startIndex(), endIndex()),
  );

  // ── Scroll handler (RAF-throttled) ─────────────────────────────
  const handleScroll = () => {
    if (rafId !== null) return;
    rafId = requestAnimationFrame(() => {
      rafId = null;
      if (scrollContainerRef) {
        setScrollTop(scrollContainerRef.scrollTop);
      }
    });
  };

  // ── Measure viewport height ────────────────────────────────────
  const measureHeight = () => {
    if (scrollContainerRef) {
      setContainerHeight(scrollContainerRef.clientHeight);
    }
  };

  let resizeObserver: ResizeObserver | undefined;

  onMount(() => {
    measureHeight();
    if (scrollContainerRef) {
      resizeObserver = new ResizeObserver(measureHeight);
      resizeObserver.observe(scrollContainerRef);
    }
  });

  onCleanup(() => {
    if (rafId !== null) cancelAnimationFrame(rafId);
    resizeObserver?.disconnect();
  });

  // ── Scroll selected task into view ─────────────────────────────
  const scrollToTask = (taskId: string) => {
    const idx = sortedTasks().findIndex((t) => t.id === taskId);
    if (idx < 0 || !scrollContainerRef) return;
    const top = idx * itemHeight();
    const bottom = top + itemHeight();
    const viewTop = scrollContainerRef.scrollTop;
    const viewBottom = viewTop + scrollContainerRef.clientHeight;
    if (top < viewTop) {
      scrollContainerRef.scrollTop = top;
    } else if (bottom > viewBottom) {
      scrollContainerRef.scrollTop = bottom - scrollContainerRef.clientHeight;
    }
  };

  // Auto-scroll when the externally-selected task changes
  createEffect(() => {
    const id = props.selectedTaskId;
    if (id) scrollToTask(id);
  });

  return (
    <div class="flex-1 flex flex-col min-w-0 overflow-hidden">
      {/* List Header */}
      <div
        class="flex items-center flex-shrink-0"
        style={{
          height: "36px",
          padding: "0 16px",
          background: "var(--color-bg-elevated)",
          "border-bottom": "1px solid var(--color-border-subtle)",
          "font-size": "12px",
          color: "var(--color-text-tertiary)",
          "font-weight": 600,
          "text-transform": "uppercase",
          "letter-spacing": "0.5px",
        }}
      >
        <For each={COLUMNS}>
          {(col) => {
            const sortState = $taskSort.state();
            const isSorted = sortState.key === col.key;
            const ariaSort = isSorted
              ? sortState.dir === "asc"
                ? "ascending"
                : "descending"
              : "none";
            const widthStyle =
              col.width === "flex-1" ? { flex: "1" } : { width: col.width };
            return (
              <div
                role="columnheader"
                aria-sort={ariaSort}
                class={
                  col.sortable
                    ? "task-col-header focus:outline-none focus-visible:focus-ring"
                    : ""
                }
                style={{
                  ...widthStyle,
                  "text-align": col.align,
                  cursor: col.sortable ? "pointer" : "default",
                  "user-select": "none",
                  display: "flex",
                  "align-items": "center",
                  "justify-content":
                    col.align === "right" ? "flex-end" : "flex-start",
                  gap: "4px",
                  "border-radius": "4px",
                }}
                onClick={() => col.sortable && toggleSort(col.key)}
                onKeyDown={(e) => {
                  if (col.sortable && (e.key === "Enter" || e.key === " ")) {
                    e.preventDefault();
                    toggleSort(col.key);
                  }
                }}
                tabindex={col.sortable ? 0 : undefined}
              >
                <span>{i18n.t(col.labelKey) as string}</span>
                <Show when={col.sortable}>
                  <span
                    style={{
                      "font-size": "9px",
                      color: isSorted
                        ? "var(--color-accent-primary)"
                        : "var(--color-text-tertiary)",
                      opacity: isSorted ? 1 : 0.4,
                    }}
                  >
                    {isSorted ? (sortState.dir === "asc" ? "▲" : "▼") : "↕"}
                  </span>
                </Show>
              </div>
            );
          }}
        </For>
      </div>

      {/* Virtual-scroll viewport */}
      <div
        ref={scrollContainerRef}
        class="flex-1 overflow-y-auto"
        onScroll={handleScroll}
      >
        {/* 屏幕阅读器实时播报任务数量变化 */}
        <div
          class="sr-only"
          role="status"
          aria-live="polite"
          aria-atomic="true"
        >
          {i18n.t("taskList.summary", { count: sortedTasks().length })}
        </div>
        <Show
          when={props.tasks.length > 0}
          fallback={
            <div class="flex flex-col items-center justify-center h-full gap-5">
              {/* 品牌抽象图形:速度粒子轨道,无渐变,纯单色调 */}
              <div
                style={{
                  width: "96px",
                  height: "96px",
                  color: "var(--color-text-tertiary)",
                  opacity: 0.25,
                  display: "flex",
                  "align-items": "center",
                  "justify-content": "center",
                }}
                aria-hidden="true"
              >
                <svg
                  width="80"
                  height="80"
                  viewBox="0 0 80 80"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.5"
                >
                  <circle cx="40" cy="40" r="30" />
                  <circle cx="40" cy="40" r="18" />
                  <circle cx="40" cy="40" r="6" fill="currentColor" />
                  <path d="M40 6 L44 14 L36 14 Z" fill="currentColor" />
                  <path d="M40 74 L44 66 L36 66 Z" fill="currentColor" />
                  <path d="M6 40 L14 44 L14 36 Z" fill="currentColor" />
                  <path d="M74 40 L66 44 L66 36 Z" fill="currentColor" />
                </svg>
              </div>
              <div class="text-center" style={{ "max-width": "320px" }}>
                <p
                  style={{
                    "font-size": "16px",
                    "font-weight": 500,
                    color: "var(--color-text-secondary)",
                    "margin-bottom": "6px",
                  }}
                >
                  暂无下载任务
                </p>
                <p
                  style={{
                    "font-size": "13px",
                    color: "var(--color-text-tertiary)",
                    "line-height": "1.5",
                    "margin-bottom": "16px",
                  }}
                >
                  新建下载任务,或拖拽链接到窗口开始体验 Tachyon 速度
                </p>
                <Show when={props.onNewTask}>
                  <Button variant="primary" size="md" onClick={props.onNewTask}>
                    <PlusIcon />
                    <span>新建下载</span>
                  </Button>
                </Show>
              </div>
              <div
                class="flex items-center gap-2 flex-wrap justify-center"
                style={{
                  "font-size": "12px",
                  color: "var(--color-text-tertiary)",
                  "margin-top": "4px",
                }}
              >
                <span class="kbd">N</span>
                <span>新建任务</span>
                <span style={{ color: "var(--color-border-strong)" }}>·</span>
                <span class="kbd">⌘</span>
                <span class="kbd">V</span>
                <span>粘贴链接</span>
                <span style={{ color: "var(--color-border-strong)" }}>·</span>
                <span>拖拽 .txt 链接文件</span>
              </div>
            </div>
          }
        >
          {/* Outer wrapper: sets total scrollable height via spacer */}
          <div style={{ position: "relative", height: `${totalHeight()}px` }}>
            {/* Inner wrapper: offset to the visible window */}
            <div
              style={{
                position: "absolute",
                top: 0,
                left: 0,
                right: 0,
                transform: `translateY(${offsetY()}px)`,
              }}
            >
              <For each={visibleTasks()}>
                {(task, visibleIndex) => (
                  <TaskItem
                    task={task}
                    isSelected={props.selectedTaskId === task.id}
                    isMultiSelected={props.selectedTaskIds.has(task.id)}
                    isMultiSelectMode={props.isMultiSelectMode}
                    onClick={() => props.onTaskClick(task.id)}
                    onContextMenu={(e) => props.onTaskContextMenu?.(e, task.id)}
                    density={props.density}
                    searchQuery={props.searchQuery}
                    staggerIndex={visibleIndex()}
                  />
                )}
              </For>
            </div>
          </div>
        </Show>
      </div>
    </div>
  );
}
