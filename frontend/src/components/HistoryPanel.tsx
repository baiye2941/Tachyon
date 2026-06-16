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
} from "./icons";
import { formatSize, formatSpeed } from "../utils/format";
import {
  historyRecords,
  getHistoryStatsForRecords,
  type HistoryRecord,
} from "../stores/history";

interface HistoryPanelProps {
  visible: boolean;
  tasks: TaskInfo[];
  onClose: () => void;
  onOpenFolder: (taskId: string) => void;
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
    savePath: "",
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
  if (days === 0) return "今天";
  if (days === 1) return "昨天";
  if (days < 7) return `${days}天前`;
  if (days < 30) return `${Math.floor(days / 7)}周前`;
  return `${Math.floor(days / 30)}月前`;
}

function getStatusLabel(status: HistoryRecord["status"]): string {
  switch (status) {
    case "completed":
      return "已完成";
    case "failed":
      return "出错";
    case "cancelled":
      return "已取消";
    default:
      return status;
  }
}

export default function HistoryPanel(props: HistoryPanelProps) {
  const [timeRange, setTimeRange] = createSignal<"7d" | "30d" | "all">("all");
  const [searchQuery, setSearchQuery] = createSignal("");

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

  return (
    <div
      class="slide-panel"
      role="dialog"
      aria-modal="true"
      aria-label="下载历史"
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
          <span>下载历史</span>
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
                  ? "近7天"
                  : range === "30d"
                    ? "近30天"
                    : "全部"}
              </button>
            )}
          </For>
          <button
            class="icon-btn-sm hover-light"
            onClick={() => props.onClose()}
            aria-label="关闭历史面板"
          >
            <CloseIcon />
          </button>
        </div>
      </div>

      <div class="flex-1 overflow-y-auto" style={{ padding: "20px" }}>
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
              总下载量
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
              任务总数
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
              平均速度
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
              最快记录
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
              成功率
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
              最大文件
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
            下载量趋势
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
                最快记录
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

        {/* Records */}
        <div class="section-label" style={{ "margin-bottom": "12px" }}>
          历史记录
        </div>
        <input
          type="text"
          placeholder="搜索历史记录..."
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
              暂无历史记录
            </div>
          }
        >
          <For each={filteredRecords()}>
            {(record) => (
              <div
                class="flex items-center gap-3 hover-row"
                style={{
                  padding: "10px 12px",
                  "border-radius": "8px",
                  transition: "all 150ms ease",
                }}
              >
                <PackageIcon />
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
                    {getStatusLabel(record.status)} ·{" "}
                    {timeAgo(record.completedAt)}
                  </div>
                </div>
                <div class="flex items-center gap-1">
                  <button
                    class="icon-btn-sm hover-light"
                    onClick={() => props.onOpenFolder(record.id)}
                    aria-label={`打开目录 ${record.fileName}`}
                  >
                    <FolderOpenIcon />
                  </button>
                  <button
                    class="icon-btn-sm hover-light"
                    onClick={() => props.onRedownload(recordToTaskInfo(record))}
                    aria-label={`重新下载 ${record.fileName}`}
                  >
                    <RefreshIcon />
                  </button>
                  <button
                    class="icon-btn-sm hover-danger"
                    onClick={() => props.onDeleteRecord(record.id)}
                    aria-label={`删除记录 ${record.fileName}`}
                  >
                    <TrashIcon />
                  </button>
                </div>
              </div>
            )}
          </For>
        </Show>
      </div>
    </div>
  );
}
