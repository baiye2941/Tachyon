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
  ChevronDownIcon,
  CancelIcon,
} from "./icons";
import { api } from "../api/invoke";
import { refreshTaskList } from "../stores/downloads";
import { addToast } from "../stores/toast";
import { requestConfirm } from "../stores/confirm";
import { useReducedMotion } from "../hooks/useReducedMotion";
import { useIsNarrowScreen } from "../hooks/useMediaQuery";
import { useFocusTrap } from "../hooks/useFocusTrap";
import { tr, type MessageKey } from "../i18n";
import SpeedChart from "./SpeedChart";
import Button from "../shared/ui/Button";
import { inferFailure, parseHfUrl, type FailureInsight } from "../utils/errorReason";
import { buildHfMirrorUrl } from "../utils/hfMirror";

const ChunkMatrix = lazy(() => import("./ChunkMatrix"));

const DIAGNOSTICS_CATEGORY_KEY: Record<FailureInsight["category"], MessageKey> = {
  network: "detail.diagnostics.category.network",
  auth: "detail.diagnostics.category.auth",
  disk: "detail.diagnostics.category.disk",
  ssl: "detail.diagnostics.category.ssl",
  cancelled: "detail.diagnostics.category.cancelled",
  unknown: "detail.diagnostics.category.unknown",
};

interface DetailPanelProps {
  task: TaskInfo | null;
  onClose: () => void;
}

export default function DetailPanel(props: DetailPanelProps) {
  const t = (key: MessageKey, values?: Record<string, string | number>) =>
    tr(key, values as Record<string, string | number | unknown>);
  const [displayTask, setDisplayTask] = createSignal<TaskInfo | null>(null);
  const [shouldRender, setShouldRender] = createSignal(false);
  const [visible, setVisible] = createSignal(false);
  const [menuOpen, setMenuOpen] = createSignal(false);
  const [copied, setCopied] = createSignal<string | null>(null);
  const [diagnosticsExpanded, setDiagnosticsExpanded] = createSignal(false);
  const [metadataExpanded, setMetadataExpanded] = createSignal(false);
  // 重试 loading 态:防止重复点击(Iteration 16)
  const [retrying, setRetrying] = createSignal(false);
  const [mirrorRetrying, setMirrorRetrying] = createSignal(false);

  // 响应式 + 动效偏好(Iteration 13)
  const isNarrow = useIsNarrowScreen();
  const reducedMotion = useReducedMotion();

  let closeTimer: number | null = null;
  let copiedTimer: number | null = null;
  let menuRef: HTMLDivElement | undefined;
  let menuTriggerRef: HTMLButtonElement | undefined;
  let panelContentRef: HTMLDivElement | undefined;

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
      setDiagnosticsExpanded(false);
      setMetadataExpanded(false);
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

  // 更多菜单:打开时聚焦首项,关闭时还原焦点到触发按钮(Iteration 15)
  createEffect(() => {
    if (!menuOpen()) return;
    const raf = requestAnimationFrame(() => {
      const first = menuRef?.querySelector<HTMLButtonElement>(".detail-menu-item");
      first?.focus();
    });
    const handler = (e: KeyboardEvent) => {
      if (!menuRef) return;
      const items = Array.from(
        menuRef.querySelectorAll<HTMLButtonElement>(".detail-menu-item"),
      ).filter((el) => !el.disabled);
      if (items.length === 0) return;
      const idx = items.findIndex((el) => el === document.activeElement);
      if (e.key === "Escape") {
        e.preventDefault();
        setMenuOpen(false);
        menuTriggerRef?.focus();
      } else if (e.key === "ArrowDown") {
        e.preventDefault();
        items[(idx + 1) % items.length]!.focus();
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        items[(idx - 1 + items.length) % items.length]!.focus();
      } else if (e.key === "Home") {
        e.preventDefault();
        items[0]!.focus();
      } else if (e.key === "End") {
        e.preventDefault();
        items[items.length - 1]!.focus();
      }
    };
    document.addEventListener("keydown", handler);
    onCleanup(() => {
      cancelAnimationFrame(raf);
      document.removeEventListener("keydown", handler);
    });
  });

  // 窄屏 sheet:focus trap + Esc 关闭(Iteration 13 遗留,Iteration 15 补齐)
  useFocusTrap({
    active: () => isNarrow() && visible(),
    container: panelContentRef,
    onEscape: () => handleClose(),
  });

  // 宽屏侧栏:Esc 关闭(非 trap,侧栏与列表共存,仅关面板)
  createEffect(() => {
    if (isNarrow() || !visible()) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !menuOpen()) {
        e.preventDefault();
        handleClose();
      }
    };
    document.addEventListener("keydown", handler);
    onCleanup(() => document.removeEventListener("keydown", handler));
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
  // cancel:立即停止但保留记录,对未终止的活跃/暂停任务可用
  const canCancel = () => {
    const s = task()?.status;
    return (
      s === "downloading" ||
      s === "connecting" ||
      s === "resuming" ||
      s === "paused"
    );
  };

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
    const t2 = currentTask();
    if (!t2) return;
    try {
      await api.pauseTask(t2.id);
      await refreshTaskList();
    } catch (e) {
      addToast(tr("toast.pauseFailed", { error: e }), "error");
    }
  };

  const handleResume = async () => {
    const t2 = currentTask();
    if (!t2) return;
    // 重试 loading 态:防止重复点击(Iteration 16)
    if (retrying() || mirrorRetrying()) return;
    setRetrying(true);
    try {
      await api.resumeTask(t2.id);
      await refreshTaskList();
      addToast(tr("toast.resumeSuccess"), "success");
    } catch (e) {
      addToast(tr("toast.resumeFailed", { error: e }), "error");
    } finally {
      setRetrying(false);
    }
  };

  const handleDelete = async () => {
    const t2 = currentTask();
    if (!t2) return;
    // Iteration 11:走应用层 ConfirmDialog(danger tone),
    // 不再触发 invoke 内置 window.confirm,与 Tachyon 品牌视觉一致。
    const result = await requestConfirm({
      title: tr("confirm.delete.title"),
      message: tr("confirm.delete.message", { name: t2.fileName }),
      confirmLabel: tr("confirm.delete.confirmLabel"),
      tone: "danger",
      showDeleteLocalFileOption: true,
      deleteLocalFileDefault: false,
    });
    if (!result.ok) return;
    try {
      await api.deleteTask(t2.id, { skipConfirm: true, deleteLocalFile: result.deleteLocalFile });
      props.onClose();
      await refreshTaskList();
    } catch (e) {
      addToast(tr("toast.deleteFailed", { error: e }), "error");
    }
  };

  // 取消任务:立即停止下载但保留记录(区别于 delete)。cancel_task 是 mutate
  // 级,后端无需 confirmation token;详情面板单任务操作,无需二次确认。
  const handleCancel = async () => {
    const t2 = currentTask();
    if (!t2) return;
    try {
      await api.cancelTask(t2.id);
      await refreshTaskList();
    } catch (e) {
      addToast(tr("toast.cancelFailed", { error: e }), "error");
    }
  };

  const handleRedownload = async () => {
    const t2 = currentTask();
    if (!t2) return;
    if (retrying() || mirrorRetrying()) return;
    setRetrying(true);
    try {
      await api.createTask(t2.url);
      await refreshTaskList();
      addToast(tr("toast.redownloadSuccess"), "success");
    } catch (e) {
      addToast(tr("toast.redownloadFailed", { error: e }), "error");
    } finally {
      setRetrying(false);
    }
  };

  /**
   * 通过 hf-mirror 镜像重试 HuggingFace 失败任务(Iteration 16)。
   *
   * 策略:
   * - 主源:hf-mirror.com 镜像 URL(基于 repoId 构造,绕过 CDN 域名差异)
   * - 容灾:原始 HF 链接(传给 mirrorUrls,主源失败时后端自动切换)
   *
   * 仅对可解析的 HuggingFace URL 显示此按钮(parseHfUrl 返回非 null)。
   */
  const handleRetryWithMirror = async () => {
    const t2 = currentTask();
    if (!t2 || !t2.url) return;
    if (retrying() || mirrorRetrying()) return;
    const parsed = parseHfUrl(t2.url);
    if (!parsed) {
      addToast(tr("toast.mirrorParseFailed"), "error");
      return;
    }
    setMirrorRetrying(true);
    try {
      const mirrorUrl = buildHfMirrorUrl(parsed.repoId, parsed.revision, parsed.filePath);
      // 镜像主源 + 原始链接容灾:与 HfBrowserPanel 镜像下载策略一致
      await api.createTask(mirrorUrl, undefined, [t2.url]);
      await refreshTaskList();
      addToast(tr("toast.mirrorRetrySuccess"), "success");
      // 镜像重试创建新任务后,关闭当前失败任务的详情面板
      handleClose();
    } catch (e) {
      addToast(tr("toast.mirrorRetryFailed", { error: e }), "error");
    } finally {
      setMirrorRetrying(false);
    }
  };

  const handleOpenFolder = async () => {
    const t2 = currentTask();
    if (!t2) return;
    if (t2.savePath) {
      try {
        await api.openFolder(t2.savePath);
      } catch {
        addToast(tr("toast.openFolderFailed"), "error");
      }
    } else {
      addToast(tr("toast.noSavePath"), "info");
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
      {/* 外层容器:窄屏 fixed 全屏 sheet + 遮罩;宽屏 grid 列宽过渡。
          内容树只写一份,通过动态 style 切换布局模式(Iteration 13)。
          窄屏用 fixed 定位天然脱离文档流,z-index 覆盖主内容,无需 Portal。 */}
      <Show when={isNarrow() && visible()}>
        <div
          class="fixed inset-0 z-[280]"
          style={{
            background: "var(--color-overlay-scrim)",
            "backdrop-filter": "blur(4px)",
            animation: reducedMotion() ? "none" : "fadeIn 150ms ease forwards",
          }}
          onClick={() => props.onClose()}
        />
      </Show>
      <div
        style={isNarrow()
          ? {
              position: "fixed",
              top: 0,
              right: 0,
              bottom: 0,
              width: "min(420px, 100vw)",
              "max-width": "100vw",
              background: "var(--color-bg-secondary)",
              "border-left": "1px solid var(--color-border-subtle)",
              "box-shadow": "var(--shadow-xl)",
              transform: visible() ? "translateX(0)" : "translateX(100%)",
              transition: reducedMotion()
                ? "none"
                : "transform 260ms cubic-bezier(0.32, 0.72, 0, 1)",
              "z-index": "290",
              overflow: "hidden",
            }
          : {
              display: "grid",
              "grid-template-columns": visible() ? "var(--panel-detail-width, 400px)" : "0px",
              "grid-template-rows": "1fr",
              transition: reducedMotion()
                ? "none"
                : "grid-template-columns 280ms cubic-bezier(0.32, 0.72, 0, 1)",
              overflow: "hidden",
              "min-height": 0,
              height: "100%",
              opacity: visible() ? 1 : 0,
              "pointer-events": visible() ? "auto" : "none",
            }
        }
      >
          <div
            ref={panelContentRef}
            role="complementary"
            aria-label={t("detail.aria")}
          tabIndex={isNarrow() ? -1 : undefined}
          style={{
            width: isNarrow() ? "100%" : "var(--panel-detail-width, 400px)",
            "max-width": "100%",
            height: "100%",
            background: "var(--color-bg-secondary)",
            "border-left": "1px solid var(--color-border-subtle)",
            transition: reducedMotion() ? "none" : "opacity 220ms ease",
          }}
          class="flex flex-col h-full overflow-y-auto overflow-x-hidden"
        >
          <div class="panel-header">
            <div class="flex items-center gap-2">
              <Show when={isCompleted()}>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={handleOpenFolder}
                >
                  <OpenFileIcon />
                  <span>{t("detail.action.open")}</span>
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
                      <span>{t("common.pause")}</span>
                    </>
                  ) : (
                    <>
                      <PlayIcon />
                      <span>{t("common.resume")}</span>
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
                  aria-label={t("detail.moreActions")}
                  aria-haspopup="menu"
                  aria-expanded={menuOpen()}
                  ref={menuTriggerRef}
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
                      <span>{t("detail.copyLink")}</span>
                    </button>
                    <button
                      class="detail-menu-item"
                      onClick={() => {
                        handleOpenFolder();
                        setMenuOpen(false);
                      }}
                    >
                      <FolderOpenIcon />
                      <span>{t("detail.openFolder")}</span>
                    </button>
                    <button
                      class="detail-menu-item"
                      onClick={() => {
                        handleRedownload();
                        setMenuOpen(false);
                      }}
                    >
                      <RefreshIcon />
                      <span>{t("detail.redownload")}</span>
                    </button>
                  </div>
                </Show>
              </div>
              <Button
                variant="ghost"
                shape="icon-sm"
                aria-label={t("detail.closeAria")}
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
                "font-size": "28px",
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
                height: "6px",
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

          {/* 失败诊断 — 可展开:分类徽标 + 标题常驻;展开显示完整 hint + 后端原文(Iteration 15)
              Iteration 16 增强:重试按钮 loading 态 + HuggingFace 镜像重试入口 */}
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
                    <div class="flex items-center gap-2">
                      <span
                        class={`detail-category-badge detail-category-badge--${insight().category}`}
                      >
                        {t(DIAGNOSTICS_CATEGORY_KEY[insight().category])}
                      </span>
                      <span
                        class="detail-error-title"
                        style={{ color: "var(--color-status-error)" }}
                      >
                        {insight().title}
                      </span>
                    </div>
                    <Show when={diagnosticsExpanded()}>
                      <p
                        id="detail-diagnostics-detail"
                        class="detail-diagnostics-hint"
                      >
                        {insight().hint}
                      </p>
                      <Show when={insight().rawReason}>
                        <div class="detail-diagnostics-backend">
                          <span class="detail-diagnostics-backend-label">
                            {t("detail.diagnostics.backend")}
                          </span>
                          <pre class="detail-diagnostics-backend-pre">
                            {insight().rawReason}
                          </pre>
                        </div>
                      </Show>
                      <Show when={(task()?.retryCount ?? 0) > 0}>
                        <p class="detail-diagnostics-retry-count">
                          {t("detail.diagnostics.retryCount", { count: task()?.retryCount ?? 0 })}
                        </p>
                      </Show>
                    </Show>
                    {/* 镜像重试入口:仅 HuggingFace 可解析链接显示(Iteration 16) */}
                    <Show when={insight().canRetryWithMirror && task()?.url && parseHfUrl(task()!.url)}>
                      <button
                        class="detail-mirror-retry-link"
                        onClick={handleRetryWithMirror}
                        disabled={retrying() || mirrorRetrying()}
                        title={t("detail.retryWithMirrorHint")}
                      >
                        {t("detail.retryWithMirror")}
                      </button>
                    </Show>
                  </div>
                  <div class="flex items-start gap-1 flex-shrink-0">
                    <Show when={insight().retryable}>
                      <Button
                        variant="secondary"
                        size="sm"
                        loading={retrying()}
                        disabled={mirrorRetrying()}
                        onClick={handleResume}
                      >
                        <RefreshIcon />
                        <span>{t("detail.retry")}</span>
                      </Button>
                    </Show>
                    <button
                      class="detail-disclosure-btn"
                      aria-expanded={diagnosticsExpanded()}
                      aria-controls="detail-diagnostics-detail"
                      aria-label={
                        diagnosticsExpanded()
                          ? t("detail.diagnostics.collapse")
                          : t("detail.diagnostics.expand")
                      }
                      onClick={() => setDiagnosticsExpanded((v) => !v)}
                    >
                      <ChevronDownIcon
                        class={`detail-disclosure-chevron${
                          diagnosticsExpanded()
                            ? " detail-disclosure-chevron--open"
                            : ""
                        }`}
                      />
                    </button>
                  </div>
                </div>
              </div>
            )}
          </Show>

          {/* Tier 2 — 活动指标:仅 downloading/paused 显示。消除冗余:
              size→进度行已显示总大小;downloaded→进度行已显示;status→文件头已显示 */}
          <Show
            when={
              isDownloading() ||
              task()?.status === "paused" ||
              task()?.status === "connecting" ||
              task()?.status === "resuming"
            }
          >
            <div
              style={{
                padding: "0 20px 12px",
                "border-top": "1px solid var(--color-border-subtle)",
              }}
            >
              <div class="detail-stat-grid" style={{ "min-width": 0 }}>
                <div class="detail-stat-cell">
                  <div class="detail-stat-label">{t("detail.label.speed")}</div>
                  <div class="detail-stat-value detail-stat-value--highlight">
                    {formatSpeed(task()?.speed || 0)}
                  </div>
                </div>
                <div class="detail-stat-cell">
                  <div class="detail-stat-label">{t("detail.label.remaining")}</div>
                  <div class="detail-stat-value detail-stat-value--highlight">
                    {eta()}
                  </div>
                </div>
                <div class="detail-stat-cell">
                  <div class="detail-stat-label">{t("detail.label.fragments")}</div>
                  <div class="detail-stat-value">
                    {`${task()?.fragmentsDone || 0}/${task()?.fragmentsTotal || 0}`}
                  </div>
                </div>
                <div class="detail-stat-cell">
                  <div class="detail-stat-label">{t("detail.label.threads")}</div>
                  <div class="detail-stat-value">
                    {/* 后端当前未下发活跃线程数,诚实展示占位 */}
                    {"—"}
                  </div>
                </div>
              </div>
            </div>
          </Show>

          {/* Tier 4 — 元数据:可折叠「更多详情」,默认收起,降低默认扫描成本 */}
          <div
            style={{
              padding: "0 20px 12px",
              "border-top": "1px solid var(--color-border-subtle)",
            }}
          >
            <button
              class="detail-disclosure-row"
              aria-expanded={metadataExpanded()}
              aria-controls="detail-metadata-detail"
              onClick={() => setMetadataExpanded((v) => !v)}
            >
              <span class="detail-disclosure-row-label">
                {t("detail.section.metadata")}
              </span>
              <ChevronDownIcon
                class={`detail-disclosure-chevron${
                  metadataExpanded() ? " detail-disclosure-chevron--open" : ""
                }`}
              />
            </button>
            <Show when={metadataExpanded()}>
              <div id="detail-metadata-detail" class="detail-metadata-detail">
                <InfoRow
                  label={t("detail.label.size")}
                  value={
                    task()?.fileSize
                      ? formatSize(task()!.fileSize!)
                      : t("common.unknown")
                  }
                />
                <InfoRow
                  label={t("detail.label.savePath")}
                  value={task()?.savePath || t("detail.savePathPending")}
                  copyable
                  copied={copied() === "path"}
                  onCopy={() => copyToClipboard(task()?.savePath || "", "path")}
                />
                <InfoRow
                  label={t("detail.label.url")}
                  value={task()?.url || ""}
                  copyable
                  copied={copied() === "url"}
                  onCopy={() => copyToClipboard(task()?.url || "", "url")}
                />
                <InfoRow
                  label={t("detail.label.createdAt")}
                  value={task()?.createdAt ? formatDate(task()!.createdAt) : "---"}
                />
              </div>
            </Show>
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

          {/* Action Buttons - 固定底部(spec 8.2),sticky 不随内容滚动 */}
          <div
            class="flex flex-col"
            style={{
              padding: "12px 20px 20px",
              gap: "8px",
              position: "sticky",
              bottom: "0",
              "margin-top": "auto",
              background: "var(--color-bg-elevated)",
              "border-top": "1px solid var(--color-border-subtle)",
            }}
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
                  ? t("detail.action.pause")
                  : t("detail.action.resume")}
              </Button>
            </Show>
            <Show when={canCancel()}>
              <Button
                variant="secondary"
                size="lg"
                class="detail-action-btn"
                fullWidth
                onClick={handleCancel}
              >
                <CancelIcon />
                {t("detail.action.cancel")}
              </Button>
            </Show>
            <Button
              variant="danger"
              size="lg"
              class="detail-action-btn"
              fullWidth
              onClick={handleDelete}
            >
              {t("detail.action.delete")}
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
          aria-label={props.copied ? tr("detail.copied.aria") : tr("detail.copy.aria")}
          title={props.copied ? tr("detail.copied.aria") : tr("detail.copy.aria")}
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
