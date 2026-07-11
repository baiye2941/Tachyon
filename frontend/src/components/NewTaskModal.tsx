import { createSignal, createMemo, createEffect, Show, For } from "solid-js";
import { CloseIcon, FolderOpenIcon, PlusIcon, XIcon, ChevronDownIcon, SearchIcon } from "./icons";
import { api } from "../api/invoke";
import { addToast } from "../stores/toast";
import { refreshTaskList } from "../stores/downloads";
import { $ui } from "../stores/ui";
import Button from "../shared/ui/Button";
import { parseUrlLines, validateUrl, extractSuggestedFileName } from "../utils/urlValidation";
import { parseDroppedFiles } from "../utils/dragDrop";
import { useFocusTrap } from "../hooks/useFocusTrap";
import { useIsSmallScreen } from "../hooks/useMediaQuery";
import { tr } from "../i18n";
import { parseHfUrl } from "../utils/hfUrl";
import { getModelInfo } from "../stores/hub";
import { batchDownload } from "../stores/model";
import type { HfModelInfo } from "../types";

interface NewTaskModalProps {
  onClose: () => void;
}

export default function NewTaskModal(props: NewTaskModalProps) {
  const isSmall = useIsSmallScreen();

  // 多行 URL 输入(textarea,支持批量粘贴)
  const [urlText, setUrlText] = createSignal("");
  // 镜像源动态行
  const [mirrors, setMirrors] = createSignal<string[]>([]);
  const [savePath, setSavePath] = createSignal("");
  const [fileName, setFileName] = createSignal("");
  const [autoStart, setAutoStart] = createSignal(true);
  const [isDragOver, setIsDragOver] = createSignal(false);
  const [creating, setCreating] = createSignal(false);
  // 高级选项(镜像源/自定义文件名)默认折叠(spec 8.6 渐进披露)
  const [advancedOpen, setAdvancedOpen] = createSignal(false);

  // ── HuggingFace 预览 ──────────────────────────────────
  const [hfPreview, setHfPreview] = createSignal<HfModelInfo | null>(null);
  let hfDebounceTimer: number | undefined;

  // ── 探测真实文件名 ──────────────────────────────────
  const [probing, setProbing] = createSignal(false);
  const [probedFilename, setProbedFilename] = createSignal<string | null>(null);

  let urlInputRef: HTMLTextAreaElement | undefined;
  let panelRef: HTMLDivElement | undefined;

  // ── URL 批量解析与实时校验 ──────────────────────────────────
  const parsedLines = createMemo(() => parseUrlLines(urlText()));
  const validUrls = createMemo(() =>
    parsedLines()
      .filter((l) => l.validation.valid)
      .map((l) => l.raw),
  );
  const validCount = createMemo(() => validUrls().length);
  const invalidCount = createMemo(() => parsedLines().length - validCount());
  const suggestedFileName = createMemo(() => {
    const url = validUrls()[0] ?? "";
    return url ? extractSuggestedFileName(url) ?? "" : "";
  });

  // URL 变化时清除探测结果(新 URL 需重新探测),同步清空已填入的文件名
  createEffect(() => {
    validUrls();
    setProbedFilename(null);
    setFileName("");
  });

  // ── HF URL 识别与预览 ──────────────────────────────────
  createEffect(() => {
    const urls = validUrls();
    if (urls.length !== 1) {
      setHfPreview(null);
      return;
    }
    const url = urls[0]!;
    const parsed = parseHfUrl(url);
    if (!parsed) {
      setHfPreview(null);
      return;
    }
    if (hfDebounceTimer) {
      window.clearTimeout(hfDebounceTimer);
    }
    hfDebounceTimer = window.setTimeout(async () => {
      try {
        const info = await getModelInfo(parsed.repoId, parsed.revision ?? undefined);
        setHfPreview(info);
      } catch {
        setHfPreview(null);
      }
    }, 300);
  });

  // 显示的文件名:优先探测结果,回退到本地提取
  const displayFilename = createMemo(() => {
    const probed = probedFilename();
    if (probed) return probed;
    return suggestedFileName();
  });

  // 探测按钮处理:探测成功后把名字填入重命名 input(可编辑,重新探测覆盖)
  const handleProbe = async () => {
    const url = validUrls()[0];
    if (!url) return;
    setProbing(true);
    try {
      const name = await api.probeFilename(url);
      setProbedFilename(name);
      setFileName(name);
    } catch {
      // 探测失败保持本地提取结果
    } finally {
      setProbing(false);
    }
  };

  // 镜像源校验(每行独立)
  const validMirrors = createMemo(() =>
    mirrors()
      .map((m) => m.trim())
      .filter((m) => m.length > 0 && validateUrl(m).valid),
  );

  useFocusTrap({
    active: true,
    container: panelRef,
    onEscape: () => props.onClose(),
  });

  const handleKeyDown = (e: KeyboardEvent) => {
    const target = e.target as HTMLElement | null;
    const isTextField =
      target?.tagName === "INPUT" ||
      target?.tagName === "TEXTAREA" ||
      target?.isContentEditable;

    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a" && isTextField) {
      return;
    }

    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
      e.preventDefault();
      handleSubmit();
    }
  };

  const handleBrowse = async () => {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({ directory: true, multiple: false });
      if (selected) {
        setSavePath(selected as string);
      }
    } catch (err) {
      console.warn(tr("newTask.folderPickerUnavailable"), err);
    }
  };

  // ── 镜像源动态行操作 ────────────────────────────────────────
  const addMirror = () => {
    setMirrors((prev) => [...prev, ""]);
    // 新行渲染后聚焦到新 input
    requestAnimationFrame(() => {
      const inputs =
        panelRef?.querySelectorAll<HTMLInputElement>("input[data-mirror]");
      inputs?.[inputs.length - 1]?.focus();
    });
  };
  const updateMirror = (i: number, val: string) =>
    setMirrors((prev) => prev.map((m, idx) => (idx === i ? val : m)));
  const removeMirror = (i: number) => {
    setMirrors((prev) => prev.filter((_, idx) => idx !== i));
    // 焦点回到 URL textarea,避免焦点丢失
    requestAnimationFrame(() => urlInputRef?.focus());
  };

  const handleSubmit = async () => {
    const urls = validUrls();
    if (urls.length === 0) {
      addToast(tr("toast.invalidUrl"), "error");
      return;
    }

    setCreating(true);
    try {
      const dir = savePath().trim() || undefined;
      const mirrorList = validMirrors().length > 0 ? validMirrors() : undefined;
      // 重命名仅单 URL 时生效,批量时忽略(避免一个名字套多个文件)
      const name =
        validCount() === 1 ? fileName().trim() || undefined : undefined;

      // 批量创建,共享 savePath/mirrors;allSettled 部分失败不阻断
      const results = await Promise.allSettled(
        urls.map((u) => api.createTask(u, dir, mirrorList, name, autoStart())),
      );

      const failed = results.filter((r) => r.status === "rejected");
      if (failed.length === 0) {
        addToast(tr("toast.tasksCreated", { count: urls.length }), "success");
      } else if (failed.length === urls.length) {
        const first = failed[0];
        const reason =
          first && first.status === "rejected" ? String(first.reason) : "";
        addToast(tr("toast.createTaskError", { error: reason }), "error");
      } else {
        const reasons = failed
          .map((r) => (r.status === "rejected" ? String(r.reason) : ""))
          .join("; ");
        addToast(
          tr("toast.batchPartial", {
            success: urls.length - failed.length,
            failed: failed.length,
            reasons,
          }),
          "info",
        );
      }

      // 刷新任务列表,确保新创建的任务立即显示
      await refreshTaskList();

      // 重置并关闭
      setUrlText("");
      setMirrors([]);
      setSavePath("");
      setFileName("");
      setAutoStart(true);
      props.onClose();
    } catch (err) {
      addToast(tr("toast.createTaskError", { error: err }), "error");
    } finally {
      setCreating(false);
    }
  };

  // 主按钮文案:动态显示任务数
  const submitLabel = createMemo(() => {
    const n = validCount();
    if (creating()) return tr("newTask.submit.creating");
    if (n === 0) return tr("newTask.submit.start");
    if (n === 1) return tr("newTask.submit.start");
    return tr("newTask.submit.startN", { count: n });
  });

  // ── HF 预览辅助函数 ──────────────────────────────────
  function formatTotalSize(info: HfModelInfo | null): string {
    if (!info) return "--";
    const files = info.siblings ?? []
    const totalBytes = files.reduce((sum, f) => sum + (f.type !== "directory" ? f.size : 0), 0);
    if (totalBytes < 1024) return `${totalBytes} B`;
    if (totalBytes < 1024 * 1024) return `${(totalBytes / 1024).toFixed(1)} KB`;
    if (totalBytes < 1024 * 1024 * 1024) return `${(totalBytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(totalBytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
  }

  function handleHfFullDownload() {
    const preview = hfPreview();
    if (!preview) return;
    const files = (preview.siblings ?? []).filter((f) => f.type !== "directory").map((f) => f.path);
    batchDownload(preview.id, files, preview.sha ?? "main");
    $ui.closeNewTaskModal();
  }

  function handleOpenInModelLibrary() {
    $ui.closeNewTaskModal();
    $ui.openHub();
  }

  return (
    <div
      class="fixed inset-0 z-[var(--z-overlay)] flex items-center justify-center"
      role="dialog"
      aria-modal="true"
      aria-labelledby="new-task-modal-title"
      style={{
        background: "var(--color-overlay-scrim)",
      }}
      onClick={() => props.onClose()}
      onKeyDown={handleKeyDown}
    >
      <div
        ref={panelRef}
        class="panel-surface"
        classList={{ "new-task-modal--narrow": isSmall() }}
        style={{
          width: "var(--panel-modal-width, 480px)",
          "border-radius": "16px",
          padding: isSmall() ? "16px" : "24px",
          /* 去 AI 味:实色 + shadow,移除 inset 装饰高光 */
          "box-shadow": "var(--shadow-xl)",
          animation: "toast-in 300ms cubic-bezier(0.32, 0.72, 0, 1)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div
          class="flex items-center justify-between"
          style={{ "margin-bottom": "20px" }}
        >
          <span
            id="new-task-modal-title"
            style={{
              "font-size": "18px",
              "font-weight": 600,
              color: "var(--color-text-title)",
            }}
          >
            {tr("newTask.title")}
          </span>
          <Button
            variant="ghost"
            shape="icon-sm"
            aria-label={tr("common.close")}
            onClick={() => props.onClose()}
          >
            <CloseIcon />
          </Button>
        </div>

        {/* URL Textarea(多行批量) */}
        <div style={{ "margin-bottom": "16px" }}>
          <label
            for="new-task-url-input"
            style={{
              display: "block",
              "font-size": "12px",
              "font-weight": 500,
              color: "var(--color-text-secondary)",
              "margin-bottom": "6px",
            }}
          >
            {tr("newTask.urlLabel")}
          </label>
          <textarea
            id="new-task-url-input"
            ref={urlInputRef}
            data-autofocus
            placeholder={tr("newTask.urlPlaceholder")}
            value={urlText()}
            onInput={(e) => setUrlText(e.currentTarget.value)}
            class={`input${isDragOver() ? " input-drag-over" : ""}`}
            style={{
              width: "100%",
              "min-height": "80px",
              "max-height": "160px",
              padding: "10px 12px",
              "font-size": "13px",
              "font-family": "var(--font-mono)",
              resize: "vertical",
            }}
            aria-invalid={invalidCount() > 0}
            aria-describedby="url-validation-hint"
            onDragOver={(e) => {
              e.preventDefault();
              setIsDragOver(true);
            }}
            onDragLeave={() => setIsDragOver(false)}
            onDrop={async (e) => {
              e.preventDefault();
              setIsDragOver(false);
              // 优先处理拖入的文件
              const fileUrls = await parseDroppedFiles(e.dataTransfer?.files);
              if (fileUrls.length > 0) {
                setUrlText(
                  (prev) => (prev ? prev + "\n" : "") + fileUrls.join("\n"),
                );
                return;
              }
              // 回退到文本
              const text =
                e.dataTransfer?.getData("text") ||
                e.dataTransfer?.getData("text/uri-list") ||
                "";
              if (text) {
                setUrlText((prev) => (prev ? prev + "\n" : "") + text.trim());
              }
            }}
          />
          {/* 实时校验反馈 */}
          <Show when={parsedLines().length > 0}>
            <div
              id="url-validation-hint"
              role="status"
              aria-live="polite"
              style={{
                "margin-top": "6px",
                "font-size": "12px",
                color:
                  invalidCount() > 0
                    ? "var(--color-warning)"
                    : "var(--color-status-completed)",
              }}
            >
              {tr("newTask.validCount", { count: validCount() })}
              <Show when={invalidCount() > 0}>
                {tr("newTask.invalidCount", { count: invalidCount() })}
              </Show>
            </div>
          </Show>

          {/* HF 模型预览卡片 */}
          <Show when={hfPreview()}>
            <div
              style={{
                padding: "12px",
                background: "var(--color-accent-soft)",
                "border-radius": "8px",
                border: "1px solid var(--color-accent-primary)",
                "margin-top": "8px",
              }}
            >
              <div style={{ "font-weight": 500 }}>
                {tr("hub.newTask.hfPreview")}: {hfPreview()?.id}
              </div>
              <div
                style={{
                  "font-size": "13px",
                  color: "var(--color-text-secondary)",
                }}
              >
                {tr("hub.newTask.hfPreviewInfo", {
                  files:
                    (hfPreview()?.siblings ?? []).filter((f) => f.type !== "directory")
                      .length,
                  size: formatTotalSize(hfPreview()),
                  framework: hfPreview()?.libraryName ?? "--",
                  license: hfPreview()?.license ?? "--",
                })}
              </div>
              <div
                style={{
                  display: "flex",
                  gap: "8px",
                  "margin-top": "8px",
                }}
              >
                <Button onClick={handleHfFullDownload}>
                  {tr("hub.action.downloadAll")}
                </Button>
                <Button
                  variant="secondary"
                  onClick={handleOpenInModelLibrary}
                >
                  {tr("hub.action.openInModelLibrary")} →
                </Button>
              </div>
            </div>
          </Show>
        </div>

        {/* 默认文件名即时显示 + 探测按钮 */}
        <Show when={displayFilename()}>
          <div
            class="flex items-center gap-2"
            style={{ "margin-bottom": "12px" }}
          >
            <span
              style={{
                "font-size": "12px",
                color: "var(--color-text-secondary)",
              }}
            >
              {validCount() === 1
                ? tr("newTask.defaultFilename", { name: displayFilename() })
                : tr("newTask.defaultFilenameBatch", { count: validCount(), name: displayFilename() })}
            </span>
            <Button
              variant="ghost"
              size="sm"
              loading={probing()}
              disabled={probing() || validCount() !== 1}
              onClick={handleProbe}
            >
              <SearchIcon />
              <span>{tr("newTask.probeFilename")}</span>
            </Button>
          </div>
        </Show>

        {/* 高级选项(镜像源/自定义文件名)渐进披露,默认收起(spec 8.6) */}
        <div style={{ "margin-bottom": "16px" }}>
          <button
            type="button"
            class="detail-disclosure-row"
            aria-expanded={advancedOpen()}
            aria-controls="new-task-advanced"
            onClick={() => setAdvancedOpen((v) => !v)}
            style={{ width: "100%", "margin-bottom": advancedOpen() ? "12px" : "0" }}
          >
            <span class="detail-disclosure-row-label">
              {tr("newTask.advanced")}
            </span>
            <ChevronDownIcon
              class={`detail-disclosure-chevron${advancedOpen() ? " detail-disclosure-chevron--open" : ""}`}
            />
          </button>
          <Show when={advancedOpen()}>
            <div id="new-task-advanced">
              {/* 镜像源(动态行,渐进披露) */}
              <div style={{ "margin-bottom": "16px" }}>
                <Show when={mirrors().length > 0}>
                  <label
                    style={{
                      display: "block",
                      "font-size": "12px",
                      "font-weight": 500,
                      color: "var(--color-text-secondary)",
                      "margin-bottom": "6px",
                    }}
                  >
                    {tr("newTask.mirrorLabel")}
                  </label>
                  <For each={mirrors()}>
                    {(mirror, i) => (
                      <div
                        class="flex items-center gap-2"
                        style={{ "margin-bottom": "6px" }}
                      >
                        <input
                          data-mirror="true"
                          type="text"
                          placeholder={tr("newTask.mirrorPlaceholder")}
                          value={mirror}
                          onInput={(e) => updateMirror(i(), e.currentTarget.value)}
                          class="input"
                          style={{
                            flex: 1,
                            padding: "8px 12px",
                            "font-size": "13px",
                          }}
                        />
                        <Button
                          variant="ghost"
                          shape="icon-sm"
                          aria-label={tr("newTask.removeMirror", { index: i() + 1 })}
                          onClick={() => removeMirror(i())}
                        >
                          <XIcon />
                        </Button>
                      </div>
                    )}
                  </For>
                </Show>
                <Button variant="ghost" size="sm" onClick={addMirror}>
                  <PlusIcon />
                  <span>{tr("newTask.addMirror")}</span>
                </Button>
              </div>

              {/* 重命名(仅单 URL 时显示,批量时一个名字套多个文件有歧义) */}
              <Show when={validCount() === 1}>
                <div style={{ "margin-bottom": "16px" }}>
                  <label
                    for="new-task-filename-input"
                    style={{
                      display: "block",
                      "font-size": "12px",
                      "font-weight": 500,
                      color: "var(--color-text-secondary)",
                      "margin-bottom": "6px",
                    }}
                  >
                    {tr("newTask.fileNameLabel")}
                  </label>
                  <input
                    id="new-task-filename-input"
                    type="text"
                    placeholder={
                      displayFilename() || tr("newTask.fileNamePlaceholder")
                    }
                    value={fileName()}
                    onInput={(e) => setFileName(e.currentTarget.value)}
                    class="input"
                    style={{
                      width: "100%",
                      padding: "10px 12px",
                      "font-size": "14px",
                    }}
                  />
                </div>
              </Show>
            </div>
          </Show>
        </div>

        {/* Save Path */}
        <div style={{ "margin-bottom": "16px" }}>
          <label
            for="new-task-save-input"
            style={{
              display: "block",
              "font-size": "12px",
              "font-weight": 500,
              color: "var(--color-text-secondary)",
              "margin-bottom": "6px",
            }}
          >
            {tr("newTask.saveTo")}
          </label>
          <div class="flex items-center gap-2">
            <input
              id="new-task-save-input"
              type="text"
              placeholder={tr("newTask.savePlaceholder")}
              value={savePath()}
              onInput={(e) => setSavePath(e.currentTarget.value)}
              class="input"
              style={{ flex: 1, padding: "10px 12px", "font-size": "14px" }}
            />
            <Button
              variant="secondary"
              size="md"
              class="flex items-center gap-1 flex-shrink-0"
              onClick={handleBrowse}
            >
              <FolderOpenIcon />
              <span>{tr("common.browse")}</span>
            </Button>
          </div>
        </div>

        {/* Auto Start */}
        <div
          class="flex items-center gap-2 cursor-pointer"
          style={{ "margin-bottom": "24px" }}
          onClick={() => setAutoStart((v) => !v)}
          role="checkbox"
          aria-checked={autoStart()}
          tabindex={0}
          onKeyDown={(e) => {
            if (e.key === " " || e.key === "Enter") {
              e.preventDefault();
              setAutoStart((v) => !v);
            }
          }}
        >
          <div
            style={{
              width: "18px",
              height: "18px",
              "border-radius": "4px",
              border: autoStart()
                ? "none"
                : "1px solid var(--color-border-default)",
              background: autoStart()
                ? "var(--color-accent-primary)"
                : "transparent",
              display: "flex",
              "align-items": "center",
              "justify-content": "center",
              transition: "all 150ms ease",
            }}
          >
            <Show when={autoStart()}>
              <svg
                width="12"
                height="12"
                viewBox="0 0 24 24"
                fill="none"
                stroke="var(--color-text-inverse)"
                stroke-width="3"
                stroke-linecap="round"
                stroke-linejoin="round"
              >
                <polyline points="20 6 9 17 4 12" />
              </svg>
            </Show>
          </div>
          <span
            style={{
              "font-size": "14px",
              color: "var(--color-text-secondary)",
            }}
          >
            {tr("newTask.autoStart")}
          </span>
        </div>

        {/* Actions */}
        <div class="flex items-center justify-end gap-3">
          <Button variant="ghost" size="md" onClick={() => props.onClose()}>
            {tr("common.cancel")}
          </Button>
          <Button
            variant="primary"
            size="md"
            disabled={validCount() === 0 || creating()}
            loading={creating()}
            onClick={handleSubmit}
          >
            <PlusIcon />
            <span>{submitLabel()}</span>
          </Button>
        </div>
      </div>
    </div>
  );
}
