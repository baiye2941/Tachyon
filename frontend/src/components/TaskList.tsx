import {
  For,
  Show,
  createSignal,
  createMemo,
  createEffect,
} from "solid-js";
import { createVirtualizer } from "@tanstack/solid-virtual";
import type { TaskInfo, ListDensity } from "../types";
import TaskItem from "./TaskItem";
import TaskGroupHeader from "./TaskGroupHeader";
import { $taskColumns } from "../stores/taskColumnsConfig";
import {
  $taskSort,
  toggleSort,
  sortTasks,
  sortGroupTasks,
  clearSort,
} from "../stores/taskSort";
import { PlusIcon, GearIcon, DownloadSimpleIcon, HubIcon, LinkIcon } from "./icons";
import EmptyState from "../shared/ui/EmptyState";
import { useI18n } from "../i18n";
import { useIsSmallScreen } from "../hooks/useMediaQuery";
import { matchKeyboardEvent } from "../stores/shortcuts";
import { openNewTaskModal } from "../stores/ui";
import { moveTask } from "../stores/downloads";
import { $onboarding, completeOnboarding } from "../stores/onboarding";
import ColumnSettings from "./ColumnSettings";
import type { ColumnDef, ColumnKey } from "./taskColumns";
import type { GroupKey } from "./taskGroups";
import { GROUP_ORDER, getTaskGroup } from "./taskGroups";
import type { GroupByMode } from "../stores/taskListView";

/** Keyboard navigation callbacks injected by parent */
interface TaskListKeyboardHandlers {
  onTaskActivate: (taskId: string, index: number) => void;
  onSelectRange: (
    anchorIndex: number,
    endIndex: number,
    orderedTaskIds: string[],
  ) => void;
  onSelectAll: () => void;
  onDeleteSelected: () => void;
}

type ListItem =
  | { type: "header"; group: GroupKey; count: number }
  | { type: "task"; task: TaskInfo; group: GroupKey };

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
  groupBy?: GroupByMode;
  onTaskClick: (
    taskId: string,
    index: number,
    shiftKey: boolean,
    orderedTaskIds: string[],
  ) => void;
  onTaskContextMenu?: (e: MouseEvent, taskId: string) => void;
  isMultiSelectMode: boolean;
  selectedTaskIds: Set<string>;
  density: ListDensity;
  searchQuery?: string;
  onNewTask?: () => void;
  keyboardHandlers: TaskListKeyboardHandlers;
}

export default function TaskList(props: TaskListProps) {
  const i18n = useI18n();
  const isSmall = useIsSmallScreen();

  let scrollContainerRef: HTMLDivElement | undefined;

  // ── Column settings dropdown ───────────────────────────────────
  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const [draggingKey, setDraggingKey] = createSignal<string | null>(null);

  // ── Drag-and-drop reorder state ────────────────────────────────
  const [draggingId, setDraggingId] = createSignal<string | null>(null);
  const [dropTargetId, setDropTargetId] = createSignal<string | null>(null);

  // ── Column resize local state (dragging) ───────────────────────
  /** 拖拽期间暂存列宽，避免每帧写 store 触发持久化/全列表重渲染 */
  const [resizingWidths, setResizingWidths] = createSignal<
    Partial<Record<ColumnKey, number>>
  >({});

  // ── Grouping: collapsed state (empty set = all expanded) ───────
  const [collapsedGroups, setCollapsedGroups] = createSignal<Set<GroupKey>>(
    new Set(),
  );

  // ── Keyboard navigation: active index in listItems ─────────────
  const [activeIndex, setActiveIndex] = createSignal<number | null>(null);
  /** Anchor index for Shift + Arrow range selection; cleared on plain navigation */
  const [rangeAnchorIndex, setRangeAnchorIndex] = createSignal<number | null>(
    null,
  );

  const itemHeight = createMemo(() => ITEM_HEIGHTS[props.density]);

  const groupByMode = createMemo(() => props.groupBy ?? "none");

  const isDraggable = createMemo(
    () => groupByMode() === "none" && $taskSort.state().key === null,
  );

  // ── Sorting / flat list abstraction ────────────────────────────
  const sortedTasks = createMemo(() =>
    sortTasks(props.tasks, $taskSort.state()),
  );

  const listItems = createMemo((): ListItem[] => {
    if (groupByMode() === "none") {
      return sortedTasks().map((task) => ({
        type: "task",
        task,
        group: getTaskGroup(task.status),
      }));
    }

    // 单 pass 构建分组映射，避免对 6 个固定分组重复 filter(O(6n))
    const groups = new Map<GroupKey, TaskInfo[]>();
    for (const task of sortedTasks()) {
      const group = getTaskGroup(task.status);
      let arr = groups.get(group);
      if (!arr) {
        arr = [];
        groups.set(group, arr);
      }
      arr.push(task);
    }

    const items: ListItem[] = [];
    for (const group of GROUP_ORDER) {
      const groupTasks = groups.get(group);
      if (!groupTasks || groupTasks.length === 0) continue;

      const sortedGroupTasks = sortGroupTasks(groupTasks, $taskSort.state());
      items.push({ type: "header", group, count: sortedGroupTasks.length });

      if (!collapsedGroups().has(group)) {
        for (const task of sortedGroupTasks) {
          items.push({ type: "task", task, group });
        }
      }
    }
    return items;
  });

  const orderedTaskIds = createMemo(() =>
    listItems()
      .filter((item): item is ListItem & { type: "task" } => item.type === "task")
      .map((item) => item.task.id),
  );

  /** Map flat listItems index → index in orderedTaskIds */
  const flatIndexToTaskIndex = createMemo(() => {
    const map = new Map<number, number>();
    const items = listItems();
    let taskIdx = 0;
    for (let i = 0; i < items.length; i++) {
      if (items[i]?.type === "task") {
        map.set(i, taskIdx++);
      }
    }
    return map;
  });

  /** Map a task id to its flat index in listItems */
  const indexOfTaskId = (id: string | null): number => {
    if (!id) return -1;
    return listItems().findIndex(
      (item) => item.type === "task" && item.task.id === id,
    );
  };

  /** Find the nearest task flat index starting from `start` in `direction` (+1/-1), skipping headers */
  const nearestTaskIndex = (start: number, direction: 1 | -1): number => {
    const items = listItems();
    let idx = start;
    while (idx >= 0 && idx < items.length) {
      if (items[idx]?.type === "task") return idx;
      idx += direction;
    }
    return -1;
  };

  /** Convert a flat index (pointing to a task item) to its index in orderedTaskIds */
  const toTaskIndex = (flatIdx: number): number =>
    flatIndexToTaskIndex().get(flatIdx) ?? -1;

  /** Ensure active index stays within bounds when list changes */
  createEffect(() => {
    const len = listItems().length;
    const idx = activeIndex();
    if (idx === null) return;
    if (len === 0) {
      setActiveIndex(null);
    } else if (idx >= len) {
      setActiveIndex(len - 1);
    }
  });

  /** Sync active index with externally selected task */
  createEffect(() => {
    const id = props.selectedTaskId;
    if (!id) return;
    const idx = indexOfTaskId(id);
    if (idx >= 0 && idx !== activeIndex()) {
      setActiveIndex(idx);
    }
  });

  /** 若当前排序列被隐藏，自动清除排序 */
  createEffect(() => {
    const visible = $taskColumns.visibleKeys();
    const sortKey = $taskSort.state().key;
    if (sortKey && !visible.includes(sortKey)) {
      clearSort();
    }
  });

  const scrollToIndex = (idx: number) => {
    virtualizer.scrollToIndex(idx, { align: "auto" });
  };

  const clearRangeAnchor = () => setRangeAnchorIndex(null);

  const moveActive = (nextIndex: number, shiftKey: boolean) => {
    const items = listItems();
    if (items.length === 0) return;

    const prevIndex = activeIndex() ?? nextIndex;
    setActiveIndex(nextIndex);
    scrollToIndex(nextIndex);

    const item = items[nextIndex];
    if (!item) return;

    if (item.type === "header") {
      // Header: selection shortcuts do nothing; range anchor cleared on plain navigation
      if (!shiftKey) {
        clearRangeAnchor();
      }
      return;
    }

    if (shiftKey) {
      const anchor = rangeAnchorIndex() ?? prevIndex;
      setRangeAnchorIndex(anchor);
      const anchorTaskFlatIndex = nearestTaskIndex(anchor, anchor <= nextIndex ? 1 : -1);
      const anchorTaskIndex = toTaskIndex(anchorTaskFlatIndex);
      const endTaskIndex = toTaskIndex(nextIndex);
      if (anchorTaskIndex >= 0 && endTaskIndex >= 0) {
        props.keyboardHandlers.onSelectRange(
          anchorTaskIndex,
          endTaskIndex,
          orderedTaskIds(),
        );
      }
    } else {
      clearRangeAnchor();
      // 单选模式下方向键同时切换选中项（符合列表常规行为）
      if (!props.isMultiSelectMode) {
        props.keyboardHandlers.onTaskActivate(item.task.id, nextIndex);
      }
    }
  };

  const handleListKeyDown = (e: KeyboardEvent) => {
    const items = listItems();
    if (items.length === 0) return;

    const idx = activeIndex() ?? -1;

    // 可配置的列表级快捷键（覆盖 → 默认）
    if (matchKeyboardEvent(e, "shortcut.list.openDetail")) {
      e.preventDefault();
      const item = items[idx];
      if (item?.type === "task") {
        setRangeAnchorIndex(idx);
        props.keyboardHandlers.onTaskActivate(item.task.id, idx);
      } else if (item?.type === "header") {
        setRangeAnchorIndex(idx);
        toggleGroupCollapsed(item.group);
      }
      return;
    }

    if (matchKeyboardEvent(e, "shortcut.list.togglePause")) {
      e.preventDefault();
      const item = items[idx];
      if (item?.type === "task") {
        setRangeAnchorIndex(idx);
        props.onTaskClick(
          item.task.id,
          idx,
          false,
          orderedTaskIds(),
        );
      }
      return;
    }

    if (matchKeyboardEvent(e, "shortcut.list.delete")) {
      e.preventDefault();
      e.stopPropagation();
      props.keyboardHandlers.onDeleteSelected();
      return;
    }

    switch (e.key) {
      case "ArrowDown": {
        e.preventDefault();
        let next: number;
        if (e.shiftKey) {
          // Shift + ArrowDown: 移动到下一个 task 项（跳过 header）
          next = nearestTaskIndex(idx < 0 ? 0 : idx + 1, 1);
          if (next < 0) next = idx < 0 ? 0 : Math.min(idx + 1, items.length - 1);
        } else {
          next = idx < 0 ? 0 : Math.min(idx + 1, items.length - 1);
        }
        moveActive(next, e.shiftKey);
        break;
      }
      case "ArrowUp": {
        e.preventDefault();
        let next: number;
        if (e.shiftKey) {
          // Shift + ArrowUp: 移动到上一个 task 项（跳过 header）
          next = nearestTaskIndex(idx < 0 ? items.length - 1 : idx - 1, -1);
          if (next < 0) next = idx < 0 ? items.length - 1 : Math.max(idx - 1, 0);
        } else {
          next = idx < 0 ? items.length - 1 : Math.max(idx - 1, 0);
        }
        moveActive(next, e.shiftKey);
        break;
      }
      case "Home": {
        e.preventDefault();
        moveActive(0, e.shiftKey);
        break;
      }
      case "End": {
        e.preventDefault();
        moveActive(items.length - 1, e.shiftKey);
        break;
      }
      case "Enter":
      case " ": {
        e.preventDefault();
        const item = items[idx];
        if (item?.type === "header") {
          toggleGroupCollapsed(item.group);
        } else if (item?.type === "task") {
          setRangeAnchorIndex(idx);
          props.onTaskClick(item.task.id, idx, e.shiftKey, orderedTaskIds());
        }
        break;
      }
      case "a":
      case "A": {
        if (e.ctrlKey || e.metaKey) {
          e.preventDefault();
          e.stopPropagation();
          clearRangeAnchor();
          props.keyboardHandlers.onSelectAll();
        }
        break;
      }
      default:
        break;
    }
  };

  const toggleGroupCollapsed = (group: GroupKey) => {
    setCollapsedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(group)) {
        next.delete(group);
      } else {
        next.add(group);
      }
      return next;
    });
  };

  // ── Virtualizer (TanStack Solid Virtual) ───────────────────────
  // 使用固定行高 + overscan,保持分组/平铺视图统一高度,简化键盘导航坐标映射。
  const virtualizer = createVirtualizer({
    get count() {
      return listItems().length;
    },
    getScrollElement: () => scrollContainerRef ?? null,
    estimateSize: () => itemHeight(),
    overscan: BUFFER_COUNT,
  });

  // Auto-scroll when the externally-selected task changes
  createEffect(() => {
    const id = props.selectedTaskId;
    if (id) {
      const idx = indexOfTaskId(id);
      if (idx >= 0) scrollToIndex(idx);
    }
  });

  // ── Column resize ──────────────────────────────────────────────
  const headerCellStyle = (col: ColumnDef) => {
    const w = resizingWidths()[col.key] ?? $taskColumns.width(col.key);
    return {
      flex: w === "flex-1" ? "1" : "0 0 auto",
      width: w === "flex-1" ? `${col.minWidth}px` : `${w}px`,
      "min-width": `${col.minWidth}px`,
    };
  };

  /** 拖拽期间优先返回本地暂存宽度，否则回退 store */
  const getColumnWidth = (key: ColumnKey): number | "flex-1" =>
    resizingWidths()[key] ?? $taskColumns.width(key);

  function startResize(e: PointerEvent, col: ColumnDef) {
    e.preventDefault();
    e.stopPropagation();

    const handle = e.currentTarget as HTMLDivElement;
    const cell = handle.parentElement as HTMLDivElement;
    const startX = e.clientX;
    const startWidth = cell.clientWidth;
    const minWidth = col.minWidth;

    setDraggingKey(col.key);
    handle.setPointerCapture(e.pointerId);

    function onPointerMove(ev: PointerEvent) {
      const delta = ev.clientX - startX;
      const newWidth = Math.max(minWidth, Math.round(startWidth + delta));
      setResizingWidths((prev) => ({ ...prev, [col.key]: newWidth }));
    }

    function onPointerUp(ev: PointerEvent) {
      const finalWidth = resizingWidths()[col.key];
      if (finalWidth !== undefined) {
        $taskColumns.setWidth(col.key, finalWidth);
      }
      setResizingWidths((prev) => {
        const next = { ...prev };
        delete next[col.key];
        return next;
      });
      setDraggingKey(null);
      try {
        handle.releasePointerCapture(ev.pointerId);
      } catch {
        /* ignore */
      }
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", onPointerUp);
    }

    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp);
  }

  const activeDescendantId = () => {
    const idx = activeIndex();
    if (idx === null) return undefined;
    const item = listItems()[idx];
    if (!item) return undefined;
    if (item.type === "header") return `task-group-header-${item.group}`;
    return `task-item-${item.task.id}`;
  };

  // ── Drag-and-drop handlers ─────────────────────────────────────
  const handleDragStart = (taskId: string) => (e: DragEvent) => {
    e.dataTransfer?.setData("text/task-id", taskId);
    if (e.dataTransfer) {
      e.dataTransfer.effectAllowed = "move";
    }
    setDraggingId(taskId);
  };

  const handleDragEnd = () => {
    setDraggingId(null);
    setDropTargetId(null);
  };

  const targetTaskIdFromEvent = (e: DragEvent): string | null => {
    const target = (e.target as HTMLElement | null)?.closest(
      "[data-task-id]",
    ) as HTMLElement | null;
    return target?.dataset.taskId ?? null;
  };

  const handleDragOver = (e: DragEvent) => {
    e.preventDefault();
    if (!draggingId()) return;
    const overId = targetTaskIdFromEvent(e);
    if (overId && overId !== draggingId()) {
      setDropTargetId(overId);
    } else {
      setDropTargetId(null);
    }
  };

  const handleDragLeave = () => {
    setDropTargetId(null);
  };

  const handleDrop = async (e: DragEvent) => {
    e.preventDefault();
    const draggedId = draggingId();
    if (!draggedId) return;
    const overId = targetTaskIdFromEvent(e);
    const beforeId = overId === draggedId ? undefined : overId ?? undefined;
    setDraggingId(null);
    setDropTargetId(null);
    await moveTask(draggedId, beforeId);
  };

  return (
    <div
      class="flex-1 flex flex-col min-w-0 overflow-hidden"
      classList={{ "task-list--narrow": isSmall() }}
    >
      {/* List Header */}
      <div class="flex items-center flex-shrink-0 task-list-header">
        <For each={$taskColumns.visibleColumns()}>
          {(col) => {
            const sortState = $taskSort.state;
            const isSorted = () => sortState().key === col.key;
            const ariaSort = () =>
              isSorted()
                ? sortState().dir === "asc"
                  ? "ascending"
                  : "descending"
                : "none";
            return (
              <div
                role="columnheader"
                {...{ scope: "col" }}
                aria-sort={ariaSort()}
                class={`task-list-col task-list-col--align-${col.align}`}
                classList={{
                  "task-list-col--sortable": col.sortable,
                  "focus:outline-none focus-visible:focus-ring": col.sortable,
                }}
                style={headerCellStyle(col)}
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
                    class="task-sort-indicator"
                    classList={{ "task-sort-indicator--active": isSorted() }}
                  >
                    {isSorted()
                      ? sortState().dir === "asc"
                        ? "▲"
                        : "▼"
                      : "↕"}
                  </span>
                </Show>
                <div
                  class="task-list-col-resize-handle"
                  classList={{
                    "task-list-col-resize-handle--dragging":
                      draggingKey() === col.key,
                  }}
                  onPointerDown={(e) => startResize(e, col)}
                />
              </div>
            );
          }}
        </For>

        <div class="relative ml-auto flex items-center">
          <button
            type="button"
            class="task-list-settings-button"
            aria-label={i18n.t("taskList.columns.title") as string}
            aria-expanded={settingsOpen()}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation();
              setSettingsOpen((v) => !v);
            }}
          >
            <GearIcon size={16} />
          </button>
          <Show when={settingsOpen()}>
            <ColumnSettings
              visibleKeys={$taskColumns.visibleKeys}
              onToggle={$taskColumns.toggleVisibility}
              onReset={$taskColumns.resetColumns}
              onClose={() => setSettingsOpen(false)}
            />
          </Show>
        </div>
      </div>

      {/* 屏幕阅读器实时播报任务数量变化：放在 listbox 外，避免破坏 listbox 的 option 语义 */}
      <div
        class="sr-only"
        role="status"
        aria-live="polite"
        aria-atomic="true"
      >
        {i18n.t("taskList.summary", { count: props.tasks.length })}
      </div>
      {/* Virtual-scroll viewport */}
      <div
        ref={scrollContainerRef}
        role="listbox"
        tabIndex={0}
        aria-label={i18n.t("taskList.aria.listbox") as string}
        aria-activedescendant={activeDescendantId()}
        class="flex-1 scroll-container focus:outline-none focus-visible:focus-ring"
        classList={{ "task-list--dragging": !!draggingId() }}
        onKeyDown={handleListKeyDown}
        onDragOver={handleDragOver}
        onDragLeave={handleDragLeave}
        onDrop={handleDrop}
      >
        <Show
          when={props.tasks.length > 0}
          fallback={
            <EmptyState
              class="h-full"
              brand
              icon={
                  <svg
                    width="80"
                    height="80"
                    viewBox="0 0 80 80"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.5"
                    stroke-linecap="round"
                    aria-hidden="true"
                  >
                    {/* 速度线 */}
                    <line
                      x1="6"
                      y1="32"
                      x2="74"
                      y2="32"
                      stroke-dasharray="2 8"
                      opacity="0.2"
                    />
                    <line
                      x1="10"
                      y1="40"
                      x2="70"
                      y2="40"
                      stroke-dasharray="4 4"
                      opacity="0.3"
                    />
                    <line
                      x1="6"
                      y1="48"
                      x2="74"
                      y2="48"
                      stroke-dasharray="2 8"
                      opacity="0.2"
                    />
                    {/* 起点闸门立柱 */}
                    <path d="M26 18 L26 62" opacity="0.3" />
                    <path d="M54 18 L54 62" opacity="0.3" />
                    <path d="M26 20 L54 20" opacity="0.2" />
                    <path d="M26 60 L54 60" opacity="0.2" />
                    {/* 中心粒子 */}
                    <circle
                      cx="40"
                      cy="40"
                      r="5"
                      fill="var(--color-accent-primary)"
                      stroke="none"
                      opacity="0.9"
                    />
                    {/* 粒子尾迹 */}
                    <path
                      d="M33 40 L20 40"
                      stroke="var(--color-accent-primary)"
                      stroke-width="2"
                      opacity="0.45"
                    />
                    <path
                      d="M31 35 L21 32"
                      stroke="var(--color-brand-teal)"
                      stroke-width="1.5"
                      opacity="0.35"
                    />
                    <path
                      d="M31 45 L21 48"
                      stroke="var(--color-brand-teal)"
                      stroke-width="1.5"
                      opacity="0.35"
                    />
                  </svg>
                }
                title={i18n.t("taskList.emptyTitle") as string}
                description={i18n.t("taskList.emptyDesc") as string}
                actionHighlight={!$onboarding.isCompleted()}
                action={{
                  label: i18n.t("taskList.emptyNewTask") as string,
                  onClick: () => {
                    completeOnboarding();
                    openNewTaskModal();
                    props.onNewTask?.();
                  },
                  icon: <PlusIcon />,
                  ariaLabel: i18n.t("taskList.emptyNewTaskAria") as string,
                }}
              >
                <div class="empty-state-onboarding">
                  <div class="empty-state-onboarding-hint">
                    <DownloadSimpleIcon size={16} />
                    <span>{i18n.t("taskList.empty.hintDragDrop") as string}</span>
                  </div>
                  <div class="empty-state-onboarding-hint">
                    <HubIcon size={16} />
                    <span>{i18n.t("taskList.empty.hintHub") as string}</span>
                  </div>
                  <div class="empty-state-onboarding-hint">
                    <LinkIcon size={16} />
                    <span>{i18n.t("taskList.empty.hintClipboard") as string}</span>
                  </div>
                </div>
            </EmptyState>
          }
        >
          {/* Outer wrapper: sets total scrollable height via spacer */}
          <div
            style={{
              position: "relative",
              height: `${virtualizer.getTotalSize()}px`,
            }}
          >
            <For each={virtualizer.getVirtualItems()}>
              {(virtualRow, visibleIndex) => {
                const item = createMemo(() => listItems()[virtualRow.index]);
                return (
                  <Show when={item()} keyed>
                    {(it) => (
                      <div
                        style={{
                          position: "absolute",
                          top: 0,
                          left: 0,
                          right: 0,
                          height: `${virtualRow.size}px`,
                          transform: `translateY(${virtualRow.start}px)`,
                        }}
                      >
                        {it.type === "task" ? (
                          <TaskItem
                            id={`task-item-${it.task.id}`}
                            task={it.task}
                            index={virtualRow.index}
                            role="option"
                            tabIndex={-1}
                            isSelected={props.selectedTaskId === it.task.id}
                            isMultiSelected={props.selectedTaskIds.has(
                              it.task.id,
                            )}
                            isMultiSelectMode={props.isMultiSelectMode}
                            isDraggable={isDraggable()}
                            onDragStart={handleDragStart(it.task.id)}
                            onDragEnd={handleDragEnd}
                            classList={{
                              "task-row--drop-target":
                                dropTargetId() === it.task.id,
                            }}
                            onClick={(shiftKey) => {
                              setActiveIndex(virtualRow.index);
                              if (!shiftKey) clearRangeAnchor();
                              props.onTaskClick(
                                it.task.id,
                                virtualRow.index,
                                shiftKey,
                                orderedTaskIds(),
                              );
                            }}
                            onContextMenu={(e) =>
                              props.onTaskContextMenu?.(e, it.task.id)
                            }
                            density={props.density}
                            searchQuery={props.searchQuery}
                            staggerIndex={visibleIndex()}
                            columnWidths={getColumnWidth}
                          />
                        ) : (
                          <TaskGroupHeader
                            group={it.group}
                            count={it.count}
                            collapsed={collapsedGroups().has(it.group)}
                            isActive={activeIndex() === virtualRow.index}
                            height={itemHeight()}
                            onToggle={() => toggleGroupCollapsed(it.group)}
                          />
                        )}
                      </div>
                    )}
                  </Show>
                );
              }}
            </For>
          </div>
        </Show>
      </div>
    </div>
  );
}
