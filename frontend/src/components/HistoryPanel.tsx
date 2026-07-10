import { createSignal, createMemo, For, Show } from "solid-js";
import type { TaskInfo } from "../types";
import {
  CloseIcon,
  HistoryIcon,
  FolderOpenIcon,
  RefreshIcon,
  TrashIcon,
  TrophyIcon,
  PackageIcon,
  CheckboxIcon,
} from "./icons";
import { formatSize, formatSpeed, getStatusLabel } from "../utils/format";
import {
  historyRecords,
  getHistoryStatsForRecords,
  type HistoryRecord,
} from "../stores/history";
import { requestConfirm } from "../stores/confirm";
import { addToast } from "../stores/toast";
import { tr, type MessageKey } from "../i18n";
import Button from "../shared/ui/Button";

interface HistoryPanelProps {
  visible: boolean;
  tasks: TaskInfo[];
  onClose: () => void;
  /** 打开所在文件夹,直接接收保存路径(非任务 id) */
  onOpenFolder: (savePath: string) => void;
  onRedownload: (task: TaskInfo) => void;
  onDeleteRecord: (taskId: string) => void;
}

function recordToTaskInfo(record: HistoryRecord): TaskInfo {
  return {
    id: record.id,
    url: record.url,
    fileName: record.fileName,
    fileSize: record.fileSize,
    downloaded: record.status === "completed" ? record.fileSize : 0,
    speed: record.avgSpeed,
    status: record.status,
    progress: record.status === "completed" ? 1 : 0,
    fragmentsTotal: 0,
    fragmentsDone: 0,
    createdAt: record.completedAt,
    savePath: record.savePath || "",
  };
}

function getDayKey(iso: string): string {
  return iso.slice(0, 10);
}

function isWithinDays(iso: string, days: number): boolean {
  const recordTime = new Date(iso).getTime();
  const cutoff = Date.now() - days * 24 * 60 * 60 * 1000;
  return recordTime >= cutoff;
}

function timeAgo(dateStr: string): string {
  const diff = Date.now() - new Date(dateStr).getTime();
  const days = Math.floor(diff / (1000 * 60 * 60 * 24));
  if (days === 0) return tr("time.today");
  if (days === 1) return tr("time.yesterday");
  if (days < 7) return tr("time.daysAgo", { n: days });
  if (days < 30) return tr("time.weeksAgo", { n: Math.floor(days / 7) });
  return tr("time.monthsAgo", { n: Math.floor(days / 30) });
}

function historyStatusLabel(status: HistoryRecord["status"]): string {
  // 复用共享状态标签(completed/failed/cancelled 共享 status.label.* keys)
  return getStatusLabel(status);
}

export default function HistoryPanel(props: HistoryPanelProps) {
  const t = (key: MessageKey, values?: Record<string, string | number>) =>
    tr(key, values as Record<string, string | number | unknown>);
  const [timeRange, setTimeRange] = createSignal<"7d" | "30d" | "all">("all");
  const [searchQuery, setSearchQuery] = createSignal("");
  // 批量选择状态:batchMode 开启后每条记录前显示复选框
  const [batchMode, setBatchMode] = createSignal(false);
  const [selectedIds, setSelectedIds] = createSignal<Set<string>>(new Set());

  const filteredRecords = createMemo(() => {
    let records = [...historyRecords];
    const range = timeRange();
    if (range === "7d") {
      records = records.filter((r) => isWithinDays(r.completedAt, 7));
    } else if (range === "30d") {
      records = records.filter((r) => isWithinDays(r.completedAt, 30));
    }
    const sq = searchQuery().trim().toLowerCase();
    if (sq) {
      records = records.filter((r) => r.fileName.toLowerCase().includes(sq));
    }
    return records;
  });

  const stats = createMemo(() => {
    return getHistoryStatsForRecords(filteredRecords());
  });

  const trendData = createMemo(() => {
    const days = 30;
    const buckets = new Map<string, number>();

    for (const r of filteredRecords()) {
      const day = getDayKey(r.completedAt);
      buckets.set(day, (buckets.get(day) || 0) + (r.fileSize || 0));
    }

    const data: { day: string; size: number }[] = [];
    for (let i = days - 1; i >= 0; i--) {
      const date = new Date(Date.now() - i * 24 * 60 * 60 * 1000);
      const dayStr = date.toISOString().slice(0, 10);
      data.push({ day: dayStr.slice(5), size: buckets.get(dayStr) || 0 });
    }
    return data;
  });

  const maxTrendSize = createMemo(() => {
    return Math.max(...trendData().map((d) => d.size), 1);
  });

  const allSelected = createMemo(() => {
    const records = filteredRecords();
    if (records.length === 0) return false;
    const sel = selectedIds();
    return records.every((r) => sel.has(r.id));
  });

  const toggleSelect = (id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const toggleSelectAll = () => {
    const records = filteredRecords();
    if (allSelected()) {
      // 取消当前过滤结果的全选(仅移除当前可见的,保留过滤外的)
      setSelectedIds((prev) => {
        const next = new Set(prev);
        records.forEach((r) => next.delete(r.id));
        return next;
      });
    } else {
      setSelectedIds((prev) => {
        const next = new Set(prev);
        records.forEach((r) => next.add(r.id));
        return next;
      });
    }
  };

  const toggleBatchMode = () => {
    const wasBatch = batchMode();
    setBatchMode((v) => !v);
    if (wasBatch) setSelectedIds(new Set<string>());
  };

  // 批量删除选中历史记录:纯前端 localStorage 操作,无后端调用
  const handleDeleteSelected = async () => {
    const ids = Array.from(selectedIds());
    if (ids.length === 0) return;

    const result = await requestConfirm({
      title: t("confirm.deleteHistoryBatch.title"),
      message: t("confirm.deleteHistoryBatch.message", { count: ids.length }),
      confirmLabel: t("confirm.delete.confirmLabel"),
      tone: "danger",
    });
    if (!result.ok) return;

    let failed = 0;
    for (const id of ids) {
      // onDeleteRecord 处理单条删除(同步到后端/清理内存),这里逐条调用
      try {
        props.onDeleteRecord(id);
      } catch {
        failed++;
      }
    }
    if (failed > 0) {
      addToast(
        t("toast.deleteBatchPartialFailed", {
          count: failed,
          error: "",
        }),
        "error",
      );
    }
    setSelectedIds(new Set<string>());
    if (selectedIds().size === 0) {
      // 全部删除成功后退出批量模式
      setBatchMode(false);
    }
  };

  return (
    <div
      class="slide-panel"
      role="dialog"
      aria-modal="true"
      aria-label={t("history.aria")}
      style={{
        width: "min(480px, calc(100vw - 32px))",
        transform: props.visible ? "translateX(0)" : "translateX(100%)",
        overflow: "hidden",
      }}
    >
      {/* Header */}
      <div class="panel-header">
        <div class="panel-title">
          <HistoryIcon />
          <span>{t("history.title")}</span>
        </div>
        <div class="flex items-center gap-2">
          <For each={["7d", "30d", "all"] as const}>
            {(range) => (
              <button
                class={
                  timeRange() === range
                    ? "pill-btn pill-btn-active"
                    : "pill-btn pill-btn-default"
                }
                onClick={() => setTimeRange(range)}
              >
                {range === "7d"
                  ? t("history.range.7d")
                  : range === "30d"
                    ? t("history.range.30d")
                    : t("history.range.all")}
              </button>
            )}
          </For>
          <button
            class="icon-btn-sm hover-light"
            onClick={() => props.onClose()}
            aria-label={t("history.closeAria")}
          >
            <CloseIcon />
          </button>
        </div>
      </div>

      <div class="flex-1 scroll-container" style={{ padding: "20px" }}>
        {/* Stats Grid */}
        <div
          style={{
            display: "grid",
            "grid-template-columns": "repeat(3, 1fr)",
            gap: "12px",
            "margin-bottom": "20px",
          }}
        >
          <div
            class="glass"
            style={{
              padding: "16px",
              "border-radius": "10px",
              "text-align": "center",
            }}
          >
            <div
              class="mono"
              style={{
                "font-size": "20px",
                "font-weight": 700,
                color: "var(--color-text-primary)",
              }}
            >
              {formatSize(stats().totalBytes)}
            </div>
            <div
              style={{
                "font-size": "11px",
                color: "var(--color-text-tertiary)",
                "margin-top": "4px",
              }}
            >
              {t("history.stat.totalBytes")}
            </div>
          </div>
          <div
            class="glass"
            style={{
              padding: "16px",
              "border-radius": "10px",
              "text-align": "center",
            }}
          >
            <div
              class="mono"
              style={{
                "font-size": "20px",
                "font-weight": 700,
                color: "var(--color-accent-secondary)",
              }}
            >
              {stats().totalDownloads}
            </div>
            <div
              style={{
                "font-size": "11px",
                color: "var(--color-text-tertiary)",
                "margin-top": "4px",
              }}
            >
              {t("history.stat.totalDownloads")}
            </div>
          </div>
          <div
            class="glass"
            style={{
              padding: "16px",
              "border-radius": "10px",
              "text-align": "center",
            }}
          >
            <div
              class="mono"
              style={{
                "font-size": "20px",
                "font-weight": 700,
                color: "var(--color-text-primary)",
              }}
            >
              {formatSpeed(stats().avgSpeed)}
            </div>
            <div
              style={{
                "font-size": "11px",
                color: "var(--color-text-tertiary)",
                "margin-top": "4px",
              }}
            >
              {t("history.stat.avgSpeed")}
            </div>
          </div>
          <div
            class="glass"
            style={{
              padding: "16px",
              "border-radius": "10px",
              "text-align": "center",
            }}
          >
            <div
              class="mono"
              style={{
                "font-size": "20px",
                "font-weight": 700,
                color: "var(--color-accent-primary)",
              }}
            >
              {formatSpeed(stats().maxSpeed)}
            </div>
            <div
              style={{
                "font-size": "11px",
                color: "var(--color-text-tertiary)",
                "margin-top": "4px",
              }}
            >
              {t("history.stat.maxSpeed")}
            </div>
          </div>
          <div
            class="glass"
            style={{
              padding: "16px",
              "border-radius": "10px",
              "text-align": "center",
            }}
          >
            <div
              class="mono"
              style={{
                "font-size": "20px",
                "font-weight": 700,
                color: "var(--color-accent-primary)",
              }}
            >
              {stats().totalDownloads > 0
                ? `${Math.round(stats().successRate * 100)}%`
                : "0%"}
            </div>
            <div
              style={{
                "font-size": "11px",
                color: "var(--color-text-tertiary)",
                "margin-top": "4px",
              }}
            >
              {t("history.stat.successRate")}
            </div>
          </div>
          <div
            class="glass"
            style={{
              padding: "16px",
              "border-radius": "10px",
              "text-align": "center",
            }}
          >
            <div
              class="truncate mono"
              style={{
                "font-size": "14px",
                "font-weight": 700,
                color: "var(--color-warning)",
              }}
            >
              {stats().maxFile
                ? formatSize(stats().maxFile!.fileSize || 0)
                : "-"}
            </div>
            <div
              style={{
                "font-size": "11px",
                color: "var(--color-text-tertiary)",
                "margin-top": "4px",
              }}
            >
              {t("history.stat.maxFile")}
            </div>
          </div>
        </div>

        {/* Trend Chart */}
        <div
          class="glass"
          style={{
            padding: "16px",
            "border-radius": "10px",
            "margin-bottom": "20px",
          }}
        >
          <div
            style={{
              "font-size": "12px",
              "font-weight": 600,
              color: "var(--color-text-tertiary)",
              "margin-bottom": "12px",
            }}
          >
            {t("history.trend")}
          </div>
          <div class="flex items-end gap-1" style={{ height: "120px" }}>
            <For each={trendData()}>
              {(item) => {
                const height = () =>
                  item.size > 0
                    ? Math.max((item.size / maxTrendSize()) * 100, 1)
                    : 1;
                return (
                  <div
                    class="flex-1 flex flex-col items-center gap-1 group"
                    style={{
                      height: "100%",
                      "justify-content": "flex-end",
                    }}
                  >
                    <div
                      style={{
                        width: "100%",
                        height: `${height()}%`,
                        "min-height": item.size > 0 ? "2px" : "1px",
                        "border-radius": "2px 2px 0 0",
                        background:
                          item.size > 0
                            ? "linear-gradient(to top, var(--color-accent-primary), var(--color-accent-secondary))"
                            : "color-mix(in srgb, var(--color-text-primary) 5%, transparent)",
                        transition: "height 300ms ease-out",
                      }}
                    />
                  </div>
                );
              }}
            </For>
          </div>
        </div>

        {/* Fun Facts */}
        <Show when={stats().maxFile}>
          <div
            class="glass"
            style={{
              padding: "14px",
              "border-radius": "8px",
              "margin-bottom": "20px",
            }}
          >
            <div
              class="flex items-center gap-2"
              style={{ color: "var(--color-accent-primary)" }}
            >
              <TrophyIcon />
              <span
                style={{
                  "font-size": "12px",
                  color: "var(--color-text-tertiary)",
                }}
              >
                {t("history.stat.maxSpeed")}
              </span>
            </div>
            <div
              class="mono"
              style={{
                "font-size": "14px",
                color: "var(--color-text-primary)",
                "margin-top": "4px",
              }}
            >
              {formatSpeed(stats().maxSpeed)} — {stats().maxFile?.fileName}
            </div>
          </div>
        </Show>

        {/* Records — 工具栏:搜索 + 批量选择切换 + 全选 */}
        <div
          class="flex items-center justify-between gap-2"
          style={{ "margin-bottom": "12px" }}
        >
          <div class="section-label">{t("history.records")}</div>
          <div class="flex items-center gap-2">
            <Show when={batchMode()}>
              <Button
                variant="ghost"
                size="sm"
                class="flex items-center gap-1"
                onClick={toggleSelectAll}
                disabled={filteredRecords().length === 0}
              >
                <CheckboxIcon checked={allSelected()} />
                <span>{t("history.selectAll")}</span>
              </Button>
            </Show>
            <Button
              variant="ghost"
              size="sm"
              class="flex items-center gap-1"
              style={{
                color: batchMode()
                  ? "var(--color-accent-primary)"
                  : "var(--color-text-tertiary)",
              }}
              onClick={toggleBatchMode}
              aria-label={t("history.batchSelect")}
            >
              <CheckboxIcon checked={batchMode()} />
              <span>{t("history.batchSelect")}</span>
            </Button>
          </div>
        </div>
        <input
          type="text"
          placeholder={t("history.searchPlaceholder")}
          value={searchQuery()}
          onInput={(e) => setSearchQuery(e.currentTarget.value)}
          class="input"
          style={{
            width: "100%",
            "margin-bottom": "12px",
            "font-size": "13px",
          }}
        />
        <Show
          when={filteredRecords().length > 0}
          fallback={
            <div style={{ color: "var(--color-text-tertiary)", "font-size": "13px" }}>
              {t("history.empty")}
            </div>
          }
        >
          <For each={filteredRecords()}>
            {(record) => {
              const isSelected = () => selectedIds().has(record.id);
              return (
                <div
                  class="flex items-center gap-3 hover-row"
                  role={batchMode() ? "checkbox" : undefined}
                  aria-checked={batchMode() ? isSelected() : undefined}
                  aria-label={
                    batchMode()
                      ? t("history.aria.selectRecord", { name: record.fileName })
                      : undefined
                  }
                  style={{
                    padding: "10px 12px",
                    "border-radius": "8px",
                    background: isSelected()
                      ? "var(--color-accent-soft)"
                      : "transparent",
                    "border-left": isSelected()
                      ? "2px solid var(--color-accent-primary)"
                      : "2px solid transparent",
                    transition: "all 150ms ease",
                  }}
                  onClick={() => {
                    if (batchMode()) toggleSelect(record.id);
                  }}
                >
                  {/* 批量模式:复选框;否则:包图标 */}
                  <Show
                    when={batchMode()}
                    fallback={<PackageIcon />}
                  >
                    <div
                      style={{
                        color: isSelected()
                          ? "var(--color-accent-primary)"
                          : "var(--color-text-tertiary)",
                        "flex-shrink": "0",
                      }}
                    >
                      <CheckboxIcon checked={isSelected()} />
                    </div>
                  </Show>
                  <div class="flex-1 min-w-0">
                    <div
                      class="truncate"
                      style={{
                        "font-size": "14px",
                        color: "var(--color-text-primary)",
                      }}
                    >
                      {record.fileName}
                    </div>
                    <div
                      style={{
                        "font-size": "12px",
                        color: "var(--color-text-tertiary)",
                      }}
                    >
                      {formatSize(record.fileSize || 0)} ·{" "}
                      {historyStatusLabel(record.status)} ·{" "}
                      {timeAgo(record.completedAt)}
                    </div>
                  </div>
                  {/* 非批量模式:显示操作按钮 */}
                  <Show when={!batchMode()}>
                    <div class="flex items-center gap-1">
                      <button
                        class="icon-btn-sm hover-light"
                        // 问题2修复:直接用 record.savePath 打开,不再按 id 查 $tasks
                        onClick={(e) => {
                          e.stopPropagation();
                          props.onOpenFolder(record.savePath || "");
                        }}
                        aria-label={t("history.aria.openFolder", { name: record.fileName })}
                      >
                        <FolderOpenIcon />
                      </button>
                      <button
                        class="icon-btn-sm hover-light"
                        onClick={(e) => {
                          e.stopPropagation();
                          props.onRedownload(recordToTaskInfo(record));
                        }}
                        aria-label={t("history.aria.redownload", { name: record.fileName })}
                      >
                        <RefreshIcon />
                      </button>
                      <button
                        class="icon-btn-sm hover-danger"
                        onClick={(e) => {
                          e.stopPropagation();
                          props.onDeleteRecord(record.id);
                        }}
                        aria-label={t("history.aria.delete", { name: record.fileName })}
                      >
                        <TrashIcon />
                      </button>
                    </div>
                  </Show>
                </div>
              );
            }}
          </For>
        </Show>
      </div>

      {/* 批量操作栏:批量模式下有选中时显示 */}
      <Show when={batchMode() && selectedIds().size > 0}>
        <div
          class="flex items-center justify-between"
          style={{
            padding: "12px 20px",
            "border-top": "1px solid var(--color-border-default)",
            background: "var(--color-bg-elevated)",
          }}
        >
          <span style={{ "font-size": "12px", color: "var(--color-text-tertiary)" }}>
            {t("batch.selectedCount", { count: selectedIds().size })}
          </span>
          <Button
            variant="danger"
            size="sm"
            onClick={handleDeleteSelected}
          >
            <TrashIcon />
            <span>{t("history.deleteSelected")}</span>
          </Button>
        </div>
      </Show>
    </div>
  );
}
