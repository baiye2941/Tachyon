import { errorMessage } from "../utils/appError";
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
import { loadTaskFragments, clearTaskFragments, getTaskFragmentData } from "../stores/taskFragments";
import {
  formatSize,
  formatSpeed,
  getFileType,
  formatETA,
  formatDate,
} from "../utils/format";
import {
  CloseIcon,
  FileIcon,
  OpenFileIcon,
  MoreIcon,
  CopyIcon,
  FolderOpenIcon,
  RefreshIcon,
  ChevronDownIcon,
  CancelIcon,
  ArrowLeftIcon,
  ArrowDownIcon,
} from "./icons";
import { api } from "../api/invoke";
import { refreshTaskList } from "../stores/downloads";
import { addToast } from "../stores/toast";
import { requestConfirm } from "../stores/confirm";
import { clearTaskHistory } from "../stores/taskSpeedHistory";
import { Motion } from "@motionone/solid";
import { useReducedMotion } from "../hooks/useReducedMotion";
import { useIsNarrowScreen } from "../hooks/useMediaQuery";
import { useFocusTrap } from "../hooks/useFocusTrap";
import { tr, type MessageKey } from "../i18n";
import SpeedChart from "./SpeedChart";
import InfoRow from "./DetailInfoRow";
import Button from "../shared/ui/Button";
import LiquidProgress from "./LiquidProgress";
import ProgressCelebration from "./ProgressCelebration";
import MetricCard from "./MetricCard";
import StatusBadge from "./StatusBadge";
import AnimatedNumber from "./AnimatedNumber";
import {
  inferFailure,
  parseHfUrl,
  type FailureInsight,
} from "../utils/errorReason";
import { buildHfMirrorUrl } from "../utils/hfMirror";

const ChunkMatrix = lazy(() => import("./ChunkMatrix"));

const DIAGNOSTICS_CATEGORY_KEY: Record<FailureInsight["category"], MessageKey> =
  {
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

  // task 变化时按需加载分片数据(DetailPanel 打开/task 切换/PlanComplete)
  // 仅当 fragmentsTotal > 0 且 store 中尚无有效数据时拉取,
  // 避免探测阶段(total=0)缓存空数据导致 PlanComplete 后无法重拉。
  createEffect(() => {
    const task = props.task;
    if (!task) return;
    if (task.fragmentsTotal === 0) return;
    const fragData = getTaskFragmentData(task.id);
    if (fragData && fragData.total > 0) return; // 已有有效数据,不重复拉
    loadTaskFragments(task.id);
  });

  // DetailPanel 关闭时清理分片数据
  onCleanup(() => {
    const task = props.task;
    if (task) clearTaskFragments(task.id);
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
      const first =
        menuRef?.querySelector<HTMLButtonElement>(".detail-menu-item");
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

  // 详情覆盖式:focus trap 在 visible 时统一激活(宽屏也 trap,因列表被遮罩盖住)
  useFocusTrap({
    active: () => visible(),
    container: panelContentRef,
    onEscape: () => handleClose(),
  });

  // 兜底 Esc 关闭(focus trap 未拦截时的键盘兜底)
  createEffect(() => {
    if (!visible()) return;
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
  const isCompleted = createMemo(() => task()?.status === "completed");
  const isFailed = createMemo(() => task()?.status === "failed");
  const isDownloading = createMemo(() => task()?.status === "downloading");
  // cancel:立即停止但保留记录,对未终止的活跃/暂停任务可用
  const canCancel = createMemo(() => {
    const s = task()?.status;
    return (
      s === "downloading" ||
      s === "connecting" ||
      s === "resuming" ||
      s === "paused"
    );
  });

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
      addToast(tr("toast.pauseFailed", { error: errorMessage(e) }), "error");
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
      addToast(tr("toast.resumeFailed", { error: errorMessage(e) }), "error");
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
      await api.deleteTask(t2.id, {
        skipConfirm: true,
        deleteLocalFile: result.deleteLocalFile,
      });
      clearTaskHistory(t2.id);
      props.onClose();
      await refreshTaskList();
    } catch (e) {
      addToast(tr("toast.deleteFailed", { error: errorMessage(e) }), "error");
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
      addToast(tr("toast.cancelFailed", { error: errorMessage(e) }), "error");
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
      addToast(tr("toast.redownloadFailed", { error: errorMessage(e) }), "error");
    } finally {
      setRetrying(false);
    }
  };

  /**
   * 通过 hf-mirror 镜像重试 HuggingFace 失败任务(Iteration 16)。
   *
   * 策略:以 hf-mirror.com 镜像 URL 作为单源重新下载。
   * 后端默认按 HubConfig.source_mode 处理源(镜像/竞速),此处仅保留显式镜像重试入口。
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
      const mirrorUrl = buildHfMirrorUrl(
        parsed.repoId,
        parsed.revision,
        parsed.filePath,
      );
      // 镜像单源重试:与 HfBrowserPanel 镜像下载策略一致
      await api.createTask(mirrorUrl);
      await refreshTaskList();
      addToast(tr("toast.mirrorRetrySuccess"), "success");
      // 镜像重试创建新任务后,关闭当前失败任务的详情面板
      handleClose();
    } catch (e) {
      addToast(tr("toast.mirrorRetryFailed", { error: errorMessage(e) }), "error");
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
      {/* 覆盖式遮罩:半透明 + 模糊,列表隐约可见但不可点(参考稿风格 + 保留下载器信息密度)
          z-70:高于 Toolbar z-2 / BatchToolbar z-50,低于 CommandPalette z-100 与模态遮罩 z-200+ */}
      <Show when={visible()}>
        <div
          class="absolute inset-0 z-[var(--z-detail-panel)] detail-scrim"
          onClick={() => props.onClose()}
        />
      </Show>
      {/* 详情面板:覆盖 main 区,从右滑入。z-80:高于遮罩 z-70。
          统一 absolute inset-0(废弃宽屏 grid 列模式),窄屏宽屏同为覆盖式,
          宽屏居中限宽,窄屏全宽。 */}
      <Motion.div
        class="detail-panel"
        classList={{ "detail-panel--narrow": isNarrow() }}
        initial={{ opacity: 0.92, x: "100%", scale: 0.98 }}
        animate={
          visible()
            ? { opacity: 1, x: 0, scale: 1 }
            : { opacity: 0.92, x: "100%", scale: 0.98 }
        }
        transition={
          reducedMotion()
            ? { duration: 0 }
            : {
                type: "spring",
                stiffness: 300,
                damping: 30,
                mass: 0.8,
              }
        }
      >
        <div
          ref={panelContentRef}
          role="complementary"
          aria-label={t("detail.aria")}
          tabIndex={isNarrow() ? -1 : undefined}
          class="detail-panel-content flex flex-col h-full scroll-container overflow-x-hidden"
        >
          <div class="panel-header">
            <div class="flex items-center gap-2 min-w-0 flex-1">
              {/* 覆盖式:返回箭头(关闭详情回列表) */}
              <Button
                variant="ghost"
                shape="icon-sm"
                aria-label={t("detail.closeAria")}
                title={t("detail.closeAria")}
                onClick={handleClose}
              >
                <ArrowLeftIcon />
              </Button>
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

          {/* File Info — 材质板 + 协议 pill + 状态 dot */}
          <div class="flex items-center gap-3 detail-file-info">
            <div
              class="flex items-center justify-center flex-shrink-0 file-icon-plate file-icon-plate--hero detail-file-icon-hero"
              style={{
                color: fileInfo().color,
                "--file-glow": `color-mix(in srgb, ${fileInfo().color} 22%, transparent)`,
              }}
            >
              {(() => {
                const Icon = fileInfo().icon;
                return <Icon size={28} />;
              })()}
            </div>
            <div class="min-w-0 flex-1">
              <div class="truncate detail-file-name">{task()?.fileName}</div>
              <div class="flex items-center gap-2 detail-file-meta">
                <span class="flex-shrink-0 detail-protocol-pill">
                  {task()?.url?.split(":")[0]?.toUpperCase() || ""}
                </span>
                <StatusBadge
                  status={task()?.status || "pending"}
                  showIcon
                  size="md"
                />
              </div>
            </div>
          </div>

          {/* Progress Theater — 大百分比 + LiquidProgress + 尺寸行 */}
          <div class="flex flex-col detail-progress-theater">
            <div class="flex items-end justify-between">
              <div
                class={`mono detail-progress-percent ${isDownloading() ? "speed-breathe" : ""}`}
                style={{
                  color: isFailed()
                    ? "var(--color-status-error)"
                    : isCompleted()
                      ? "var(--color-status-completed)"
                      : "var(--color-accent-primary)",
                }}
              >
                <AnimatedNumber
                  value={((task()?.progress || 0) * 100).toFixed(1)}
                />
                %
              </div>
              <Show when={isCompleted()}>
                <ProgressCelebration reducedMotion={reducedMotion()} />
              </Show>
            </div>

            <LiquidProgress
              progress={task()?.progress || 0}
              status={task()?.status || "pending"}
              size="lg"
              showStateIcon
              reducedMotion={reducedMotion()}
              class="detail-liquid-progress"
              aria-label={t("detail.label.status")}
            />

            <div class="detail-progress-row">
              <span class="mono detail-progress-size">
                {formatSize(task()?.downloaded || 0)}
                <span class="detail-progress-size-total">
                  / {formatSize(task()?.fileSize || 0)}
                </span>
              </span>
              <Show when={isDownloading()}>
                <span class="mono detail-progress-speed">
                  {formatSpeed(task()?.speed || 0)}
                </span>
              </Show>
            </div>
          </div>

          {/* 失败诊断 — 可展开:分类徽标 + 标题常驻;展开显示完整 hint + 后端原文(Iteration 15)
              Iteration 16 增强:重试按钮 loading 态 + HuggingFace 镜像重试入口 */}
          <Show when={isFailed() && failureInsight()}>
            {(insight) => (
              <div style={{ padding: "0 20px 12px" }}>
                <div
                  class="detail-error-box error-shake"
                  classList={{ "error-shake--reduced": reducedMotion() }}
                  role="alert"
                  aria-live="assertive"
                >
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
                          {t("detail.diagnostics.retryCount", {
                            count: task()?.retryCount ?? 0,
                          })}
                        </p>
                      </Show>
                    </Show>
                    {/* 镜像重试入口:仅 HuggingFace 可解析链接显示(Iteration 16) */}
                    <Show
                      when={
                        insight().canRetryWithMirror &&
                        task()?.url &&
                        parseHfUrl(task()!.url)
                      }
                    >
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

          {/* Activity Metrics — 2x2 bento cards */}
          <Show
            when={
              isDownloading() ||
              task()?.status === "paused" ||
              task()?.status === "connecting" ||
              task()?.status === "resuming"
            }
          >
            <div class="detail-section">
              <div class="metric-grid">
                <MetricCard
                  label={t("detail.label.speed")}
                  value={formatSpeed(task()?.speed || 0)}
                  highlight={isDownloading()}
                  icon={<ArrowDownIcon aria-hidden="true" />}
                />
                <MetricCard
                  label={t("detail.label.remaining")}
                  value={eta()}
                  highlight={isDownloading()}
                />
                <MetricCard
                  label={t("detail.label.fragments")}
                  value={`${task()?.fragmentsDone || 0}/${task()?.fragmentsTotal || 0}`}
                />
                <MetricCard
                  label={t("detail.label.threads")}
                  value="—"
                  hint={t("detail.label.threads")}
                />
              </div>
            </div>
          </Show>

          {/* Tier 4 — 元数据:可折叠「更多详情」,默认收起,降低默认扫描成本 */}
          <div class="detail-section detail-section--bordered">
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
                  value={
                    task()?.createdAt ? formatDate(task()!.createdAt) : "---"
                  }
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
            <div class="detail-section">
              <SpeedChart task={task()!} />
            </div>
          </Show>

          {/* Chunk Matrix - collapsible, after chart */}
          <Show when={(task()?.fragmentsTotal || 0) > 0}>
            <div class="detail-section">
              <Suspense
                fallback={
                  <div class="animate-pulse bg-white/5 rounded-lg h-full" />
                }
              >
                <ChunkMatrix
                  taskId={task()!.id}
                  fragmentsTotal={task()!.fragmentsTotal}
                  fragmentsDone={task()!.fragmentsDone}
                  progress={task()!.progress}
                />
              </Suspense>
            </div>
          </Show>

          {/* Action Buttons - 固定底部(spec 8.2),sticky 不随内容滚动 */}
          <div class="flex flex-col detail-actions">
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
      </Motion.div>
    </Show>
  );
}
