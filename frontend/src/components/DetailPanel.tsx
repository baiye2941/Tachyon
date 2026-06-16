import {
  createMemo,
  createSignal,
  createEffect,
  Show,
  onCleanup,
  untrack,
  lazy,
  Suspense,
} from "solid-js";
import type { TaskInfo } from "../types";
import {
  formatSize,
  formatSpeed,
  getFileType,
  getStatusLabel,
  getStatusColor,
  formatETA,
  formatDate,
} from "../utils/format";
import {
  CloseIcon,
  PauseIcon,
  PlayIcon,
  FileIcon,
  OpenFileIcon,
  MoreIcon,
  CopyIcon,
  FolderOpenIcon,
  RefreshIcon,
} from "./icons";
import { api } from "../api/invoke";
import { refreshTaskList } from "../stores/downloads";
import { addToast } from "../stores/toast";
import SpeedChart from "./SpeedChart";
import Button from "../shared/ui/Button";
import { inferFailure } from "../utils/errorReason";

const ChunkMatrix = lazy(() => import("./ChunkMatrix"));

interface DetailPanelProps {
  task: TaskInfo | null;
  onClose: () => void;
}

export default function DetailPanel(props: DetailPanelProps) {
  const [displayTask, setDisplayTask] = createSignal<TaskInfo | null>(null);
  const [shouldRender, setShouldRender] = createSignal(false);
  const [visible, setVisible] = createSignal(false);
  const [menuOpen, setMenuOpen] = createSignal(false);
  const [copied, setCopied] = createSignal<string | null>(null);

  let closeTimer: number | null = null;
  let copiedTimer: number | null = null;
  let menuRef: HTMLDivElement | undefined;

  const cancelCloseTimer = () => {
    if (closeTimer !== null) {
      clearTimeout(closeTimer);
      closeTimer = null;
    }
  };

  createEffect(() => {
    const task = props.task;
    if (task) {
      cancelCloseTimer();
      setDisplayTask(task);
      setMenuOpen(false);
      if (!shouldRender()) {
        setShouldRender(true);
        requestAnimationFrame(() => {
          requestAnimationFrame(() => {
            setVisible(true);
          });
        });
      } else {
        setVisible(true);
      }
    } else if (shouldRender() && visible()) {
      setVisible(false);
      cancelCloseTimer();
      closeTimer = window.setTimeout(() => {
        setShouldRender(false);
        setDisplayTask(null);
        closeTimer = null;
      }, 300);
    }
  });

  // Click outside to close menu
  createEffect(() => {
    if (!menuOpen()) return;
    const handler = (e: MouseEvent) => {
      if (menuRef && !menuRef.contains(e.target as Node)) {
        setMenuOpen(false);
      }
    };
    document.addEventListener("mousedown", handler);
    onCleanup(() => document.removeEventListener("mousedown", handler));
  });

  const handleClose = () => {
    setVisible(false);
    cancelCloseTimer();
    closeTimer = window.setTimeout(() => {
      setShouldRender(false);
      setDisplayTask(null);
      closeTimer = null;
      untrack(() => props.onClose());
    }, 300);
  };

  const task = () => displayTask();
  const fileInfo = createMemo(() => {
    const currentTask = task();
    return currentTask
      ? getFileType(currentTask.fileName)
      : { icon: FileIcon, color: "var(--color-file-other)" };
  });
  const isCompleted = () => task()?.status === "completed";
  const isFailed = () => task()?.status === "failed";
  const isDownloading = () => task()?.status === "downloading";

  // 失败诊断:优先用后端 errorReason,回退到启发式推断(诚实降级)
  const failureInsight = createMemo(() => {
    const t = task();
    if (!t || !isFailed()) return null;
    return inferFailure(t);
  });

  const eta = createMemo(() => {
    const t = task();
    if (!t || !isDownloading()) return "---";
    const remaining = (t.fileSize || 0) - t.downloaded;
    return formatETA(t.speed, remaining);
  });

  const copyToClipboard = (text: string, label: string) => {
    navigator.clipboard.writeText(text);
    setCopied(label);
    if (copiedTimer !== null) clearTimeout(copiedTimer);
    copiedTimer = window.setTimeout(() => {
      setCopied(null);
      copiedTimer = null;
    }, 1500);
  };

  const currentTask = () => task();

  const handlePause = async () => {
    const t = currentTask();
    if (!t) return;
    try {
      await api.pauseTask(t.id);
      await refreshTaskList();
    } catch (e) {
      addToast(`暂停失败: ${e}`, "error");
    }
  };

  const handleResume = async () => {
    const t = currentTask();
    if (!t) return;
    try {
      await api.resumeTask(t.id);
      await refreshTaskList();
    } catch (e) {
      addToast(`恢复失败: ${e}`, "error");
    }
  };

  const handleDelete = async () => {
    const t = currentTask();
    if (!t) return;
    try {
      await api.deleteTask(t.id);
      props.onClose();
      await refreshTaskList();
    } catch (e) {
      addToast(`删除失败: ${e}`, "error");
    }
  };

  const handleRedownload = async () => {
    const t = currentTask();
    if (!t) return;
    try {
      await api.createTask(t.url);
      await refreshTaskList();
    } catch (e) {
      addToast(`重新下载失败: ${e}`, "error");
    }
  };

  const handleOpenFolder = async () => {
    const t = currentTask();
    if (!t) return;
    if (t.savePath) {
      try {
        await api.openFolder(t.savePath);
      } catch {
        addToast("打开文件夹失败", "error");
      }
    } else {
      addToast("该任务暂无保存路径信息", "info");
    }
  };

  const handlePrimaryAction = () => {
    if (isDownloading()) {
      handlePause();
    } else {
      handleResume();
    }
  };

  return (
    <Show when={shouldRender()}>
      <div
          style={{
            display: "grid",
            "grid-template-columns": visible() ? "var(--panel-detail-width, 400px)" : "0px",
            "grid-template-rows": "1fr",
            transition:
              "grid-template-columns 280ms cubic-bezier(0.32, 0.72, 0, 1)",
            overflow: "hidden",
            "min-height": 0,
            height: "100%",
            opacity: visible() ? 1 : 0,
            "pointer-events": visible() ? "auto" : "none",
          }}
      >
        <div
          role="complementary"
          aria-label="任务详情"
          style={{
            width: "var(--panel-detail-width, 400px)",
            "max-width": "100%",
            background: "var(--color-bg-secondary)",
            "border-left": "1px solid var(--color-border-subtle)",
            transition: "opacity 220ms ease",
          }}
          class="flex flex-col h-full overflow-y-auto overflow-x-hidden"
        >
          {/* Header - compact */}
          <div class="panel-header">
            <div class="flex items-center gap-2">
              <Show when={isCompleted()}>
                <Button variant="secondary" size="sm" onClick={handleOpenFolder}>
                  <OpenFileIcon />
                  <span>{"\u6253\u5F00"}</span>
                </Button>
              </Show>
              <Show when={!isCompleted()}>
                <Button
                  variant="primary"
                  size="sm"
                  class="hover-lift-sm"
                  onClick={handlePrimaryAction}
                >
                  {isDownloading() ? (
                    <>
                      <PauseIcon />
                      <span>{"\u6682\u505C"}</span>
                    </>
                  ) : (
                    <>
                      <PlayIcon />
                      <span>{"\u6062\u590D"}</span>
                    </>
                  )}
                </Button>
              </Show>
            </div>
            <div class="flex items-center gap-1">
              <div style={{ position: "relative" }}>
                <Button
                  variant="ghost"
                  shape="icon-sm"
                  aria-label="更多操作"
                  onClick={() => setMenuOpen((v) => !v)}
                >
                  <MoreIcon />
                </Button>
                <Show when={menuOpen()}>
                  <div class="detail-menu" ref={menuRef}>
                    <button
                      class="detail-menu-item"
                      onClick={() => {
                        copyToClipboard(task()?.url || "", "url");
                        setMenuOpen(false);
                      }}
                    >
                      <CopyIcon />
                      <span>{"\u590D\u5236\u94FE\u63A5"}</span>
                    </button>
                    <button
                      class="detail-menu-item"
                      onClick={() => {
                        handleOpenFolder();
                        setMenuOpen(false);
                      }}
                    >
                      <FolderOpenIcon />
                      <span>{"\u6253\u5F00\u6587\u4EF6\u5939"}</span>
                    </button>
                    <button
                      class="detail-menu-item"
                      onClick={() => {
                        handleRedownload();
                        setMenuOpen(false);
                      }}
                    >
                      <RefreshIcon />
                      <span>{"\u91CD\u65B0\u4E0B\u8F7D"}</span>
                    </button>
                  </div>
                </Show>
              </div>
              <Button
                variant="ghost"
                shape="icon-sm"
                aria-label="关闭详情"
                onClick={handleClose}
              >
                <CloseIcon />
              </Button>
            </div>
          </div>

          {/* File Info - compact inline layout */}
          <div
            class="flex items-center gap-3"
            style={{ padding: "12px 20px", "max-width": "100%" }}
          >
            <div
              class="flex items-center justify-center flex-shrink-0"
              style={{
                width: "36px",
                height: "36px",
                color: fileInfo().color,
              }}
            >
              {(() => {
                const Icon = fileInfo().icon;
                return <Icon />;
              })()}
            </div>
            <div class="min-w-0 flex-1">
              <div
                class="truncate"
                style={{
                  "font-size": "14px",
                  "font-weight": 600,
                  color: "var(--color-text-title)",
                  "max-width": "100%",
                }}
              >
                {task()?.fileName}
              </div>
              <div
                class="flex items-center gap-2"
                style={{ "margin-top": "2px" }}
              >
                <span
                  class="flex-shrink-0"
                  style={{
                    "font-size": "10px",
                    color: "var(--color-text-tertiary)",
                    padding: "1px 6px",
                    "border-radius": "3px",
                    background: "var(--color-bg-hover)",
                  }}
                >
                  {task()?.url?.split(":")[0]?.toUpperCase() || ""}
                </span>
                <span
                  style={{
                    "font-size": "11px",
                    color: getStatusColor(task()?.status || ""),
                    "font-weight": 600,
                  }}
                >
                  {getStatusLabel(task()?.status || "")}
                </span>
              </div>
            </div>
          </div>

          {/* Progress Section - compact */}
          <div
            class="flex flex-col items-center"
            style={{ padding: "0 20px 12px" }}
          >
            <div
              class="mono"
              style={{
                "font-size": "24px",
                "font-weight": 700,
                color: "var(--color-text-title)",
                "line-height": "1.2",
              }}
            >
              {((task()?.progress || 0) * 100).toFixed(1)}%
            </div>

            {/* Progress bar */}
            <div
              class="relative overflow-hidden w-full"
              style={{
                height: "4px",
                "margin-top": "8px",
                "border-radius": "9999px",
                background: "var(--color-bg-tertiary)",
              }}
            >
              <div
                class={`absolute left-0 top-0 bottom-0${isDownloading() ? " progress-bar-active" : ""}`}
                style={{
                  width: `${(task()?.progress || 0) * 100}%`,
                  "border-radius": "9999px",
                  background: isFailed()
                    ? "var(--color-status-error)"
                    : isCompleted()
                      ? "var(--color-status-completed)"
                      : isDownloading()
                        ? undefined
                        : "linear-gradient(90deg, var(--color-accent-primary) 0%, var(--color-accent-tertiary) 100%)",
                  transition: "width 300ms ease-out",
                }}
              />
            </div>

            {/* Progress stats row */}
            <div class="detail-progress-row">
              <span
                class="mono"
                style={{ "font-size": "11px", color: "var(--color-text-secondary)" }}
              >
                {formatSize(task()?.downloaded || 0)}
              </span>
              <span
                class="mono"
                style={{ "font-size": "11px", color: "var(--color-text-tertiary)" }}
              >
                {formatSize(task()?.fileSize || 0)}
              </span>
            </div>
          </div>

          {/* Error State - 真实诊断,不硬编码错误文案 */}
          <Show when={isFailed() && failureInsight()}>
            {(insight) => (
              <div style={{ padding: "0 20px 12px" }}>
                <div class="detail-error-box" role="alert" aria-live="assertive">
                  <div class="detail-error-icon">
                    <span
                      style={{
                        color: "var(--color-status-error)",
                        "font-size": "12px",
                        "font-weight": 700,
                      }}
                    >
                      !
                    </span>
                  </div>
                  <div class="flex-1 min-w-0">
                    <div
                      style={{
                        "font-size": "13px",
                        color: "var(--color-status-error)",
                        "font-weight": 500,
                      }}
                    >
                      {insight().title}
                    </div>
                    <div
                      class="truncate"
                      style={{
                        "font-size": "12px",
                        color: "var(--color-text-secondary)",
                        "margin-top": "2px",
                      }}
                    >
                      {insight().hint}
                    </div>
                  </div>
                  <Show when={insight().retryable}>
                    <Button
                      variant="secondary"
                      size="sm"
                      class="flex-shrink-0"
                      onClick={handleResume}
                    >
                      <RefreshIcon />
                      <span>重试</span>
                    </Button>
                  </Show>
                </div>
              </div>
            )}
          </Show>

          {/* Stats Grid - 3 columns, moved UP before charts */}
          <div
            style={{
              padding: "0 20px 12px",
              "border-top": "1px solid var(--color-border-subtle)",
            }}
          >
            <div class="detail-stat-grid" style={{ "min-width": 0 }}>
              <div class="detail-stat-cell">
                <div class="detail-stat-label">{"\u5927\u5C0F"}</div>
                <div class="detail-stat-value">
                  {task()?.fileSize
                    ? formatSize(task()!.fileSize!)
                    : "\u672A\u77E5"}
                </div>
              </div>
              <div class="detail-stat-cell">
                <div class="detail-stat-label">{"\u901F\u5EA6"}</div>
                <div
                  class={`detail-stat-value${isDownloading() ? " detail-stat-value--highlight" : ""}`}
                >
                  {formatSpeed(task()?.speed || 0)}
                </div>
              </div>
              <div class="detail-stat-cell">
                <div class="detail-stat-label">{"\u5269\u4F59"}</div>
                <div
                  class={`detail-stat-value${isDownloading() ? " detail-stat-value--highlight" : ""}`}
                >
                  {eta()}
                </div>
              </div>
              <div class="detail-stat-cell">
                <div class="detail-stat-label">{"\u5206\u7247"}</div>
                <div class="detail-stat-value">{`${task()?.fragmentsDone || 0}/${task()?.fragmentsTotal || 0}`}</div>
              </div>
              <div class="detail-stat-cell">
                <div class="detail-stat-label">{"\u5DF2\u4E0B\u8F7D"}</div>
                <div class="detail-stat-value">
                  {formatSize(task()?.downloaded || 0)}
                </div>
              </div>
              <div class="detail-stat-cell">
                <div class="detail-stat-label">{"\u72B6\u6001"}</div>
                <div class="detail-stat-value">
                  {getStatusLabel(task()?.status || "")}
                </div>
              </div>
            </div>
          </div>

          {/* URL & Path - compact */}
          <div style={{ padding: "0 20px 12px" }}>
            <InfoRow
              label="下载链接"
              value={task()?.url || ""}
              copyable
              copied={copied() === "url"}
              onCopy={() => copyToClipboard(task()?.url || "", "url")}
            />
            <InfoRow
              label="保存路径"
              value={task()?.savePath || "下载尚未开始,路径待确定"}
              copyable
              copied={copied() === "path"}
              onCopy={() => copyToClipboard(task()?.savePath || "", "path")}
            />
            <InfoRow
              label="创建时间"
              value={task()?.createdAt ? formatDate(task()!.createdAt) : "---"}
            />
          </div>

          {/* Speed Chart - collapsible, after stats */}
          <Show
            when={
              task()?.status === "downloading" || task()?.status === "paused"
            }
          >
            <div style={{ padding: "0 20px 12px" }}>
              <SpeedChart task={task()!} />
            </div>
          </Show>

          {/* Chunk Matrix - collapsible, after chart */}
          <Show when={(task()?.fragmentsTotal || 0) > 0}>
            <div style={{ padding: "0 20px 12px" }}>
              <Suspense fallback={<div class="animate-pulse bg-white/5 rounded-lg h-full" />}>
                <ChunkMatrix
                  fragmentsTotal={task()!.fragmentsTotal}
                  fragmentsDone={task()!.fragmentsDone}
                  progress={task()!.progress}
                />
              </Suspense>
            </div>
          </Show>

          {/* Action Buttons - at bottom */}
          <div
            class="flex flex-col"
            style={{ padding: "0 20px 20px", gap: "8px" }}
          >
            <Show when={!isCompleted()}>
              <Button
                variant="primary"
                size="lg"
                class="hover-lift detail-action-btn"
                fullWidth
                onClick={handlePrimaryAction}
              >
                {isDownloading()
                  ? "\u6682\u505C\u4E0B\u8F7D"
                  : "\u6062\u590D\u4E0B\u8F7D"}
              </Button>
            </Show>
            <Button
              variant="danger"
              size="lg"
              class="detail-action-btn"
              fullWidth
              onClick={handleDelete}
            >
              {"\u5220\u9664\u4EFB\u52A1"}
            </Button>
          </div>
        </div>
      </div>
    </Show>
  );
}

function InfoRow(props: {
  label: string;
  value: string;
  copyable?: boolean;
  copied?: boolean;
  onCopy?: () => void;
}) {
  return (
    <div class="detail-info-row">
      <div class="min-w-0 flex-1 overflow-hidden">
        <div class="detail-info-label">{props.label}</div>
        <div class="detail-info-value">{props.value}</div>
      </div>
      <Show when={props.copyable}>
        <button
          class="icon-btn-sm"
          style={{ "flex-shrink": 0, width: "24px", height: "24px" }}
          onClick={() => props.onCopy?.()}
          title={props.copied ? "\u5DF2\u590D\u5236" : "\u590D\u5236"}
        >
          <Show when={props.copied} fallback={<CopyIcon />}>
            <span
              style={{
                color: "var(--color-accent-primary)",
                "font-size": "12px",
                "font-weight": 700,
              }}
            >
              &#10003;
            </span>
          </Show>
        </button>
      </Show>
    </div>
  );
}
