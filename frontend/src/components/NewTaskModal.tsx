import { createSignal, createMemo, Show, For } from "solid-js";
import { CloseIcon, FolderOpenIcon, PlusIcon, XIcon } from "./icons";
import { api } from "../api/invoke";
import { addToast } from "../stores/toast";
import Button from "../shared/ui/Button";
import { parseUrlLines, validateUrl } from "../utils/urlValidation";
import { parseDroppedFiles } from "../utils/dragDrop";
import { useFocusTrap } from "../hooks/useFocusTrap";

interface NewTaskModalProps {
  onClose: () => void;
}

export default function NewTaskModal(props: NewTaskModalProps) {
  // 多行 URL 输入(textarea,支持批量粘贴)
  const [urlText, setUrlText] = createSignal("");
  // 镜像源动态行
  const [mirrors, setMirrors] = createSignal<string[]>([]);
  const [savePath, setSavePath] = createSignal("");
  const [autoStart, setAutoStart] = createSignal(true);
  const [isDragOver, setIsDragOver] = createSignal(false);
  const [creating, setCreating] = createSignal(false);

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

  // Ctrl+Enter 提交(焦点在面板内时)
  const handleKeyDown = (e: KeyboardEvent) => {
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
      console.warn("文件夹选择不可用(浏览器或 SSR 环境):", err);
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
      addToast("请输入有效的下载链接", "error");
      return;
    }

    setCreating(true);
    try {
      const dir = savePath().trim() || undefined;
      const mirrorList = validMirrors().length > 0 ? validMirrors() : undefined;

      // 批量创建,共享 savePath/mirrors;allSettled 部分失败不阻断
      const results = await Promise.allSettled(
        urls.map((u) => api.createTask(u, dir, mirrorList)),
      );

      // autoStart 过渡容错:后端创建即启动,取消自动开始时延迟 pause 消除竞态
      // TODO: 后端 create_task 添加 auto_start 参数后,改为直接传入,移除此 hack
      if (!autoStart()) {
        results.forEach((r) => {
          if (r.status === "fulfilled") {
            window.setTimeout(() => {
              api.pauseTask(r.value).catch(() => {
                /* 容错:pause 失败则任务继续下载(可接受降级) */
              });
            }, 500);
          }
        });
      }

      const failed = results.filter((r) => r.status === "rejected");
      if (failed.length === 0) {
        addToast(`已创建 ${urls.length} 个任务`, "success");
      } else if (failed.length === urls.length) {
        addToast("创建任务失败", "error");
      } else {
        addToast(
          `${urls.length - failed.length} 成功, ${failed.length} 失败`,
          "info",
        );
      }

      // 重置并关闭
      setUrlText("");
      setMirrors([]);
      setSavePath("");
      setAutoStart(true);
      props.onClose();
    } catch (err) {
      addToast(`创建任务失败: ${err}`, "error");
    } finally {
      setCreating(false);
    }
  };

  // 主按钮文案:动态显示任务数
  const submitLabel = createMemo(() => {
    const n = validCount();
    if (creating()) return "创建中...";
    if (n === 0) return "开始下载";
    if (n === 1) return "开始下载";
    return `开始下载(${n})`;
  });

  return (
    <div
      class="fixed inset-0 z-[200] flex items-center justify-center"
      role="dialog"
      aria-modal="true"
      aria-labelledby="new-task-modal-title"
      style={{
        background: "var(--color-overlay-scrim)",
        "backdrop-filter": "blur(4px)",
      }}
      onClick={() => props.onClose()}
      onKeyDown={handleKeyDown}
    >
      <div
        ref={panelRef}
        class="panel-surface"
        style={{
          width: "var(--panel-modal-width, 480px)",
          "border-radius": "16px",
          padding: "24px",
          "box-shadow": "var(--shadow-xl), var(--shadow-glow)",
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
            添加下载任务
          </span>
          <Button
            variant="ghost"
            shape="icon-sm"
            aria-label="关闭"
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
            下载链接(支持多行批量添加)
          </label>
          <textarea
            id="new-task-url-input"
            ref={urlInputRef}
            data-autofocus
            placeholder={"粘贴下载链接,每行一个\n支持拖拽 .txt 文件"}
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
              {validCount()} 个有效链接
              <Show when={invalidCount() > 0}>
                , {invalidCount()} 个无效将被忽略
              </Show>
            </div>
          </Show>
        </div>

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
              镜像源(主源失败时自动切换)
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
                    placeholder="https://mirror.example.com/same-file.gguf"
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
                    aria-label={`移除镜像源 ${i() + 1}`}
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
            <span>添加镜像源</span>
          </Button>
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
            保存到
          </label>
          <div class="flex items-center gap-2">
            <input
              id="new-task-save-input"
              type="text"
              placeholder="默认下载目录"
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
              <span>浏览</span>
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
            自动开始下载
          </span>
        </div>

        {/* Actions */}
        <div class="flex items-center justify-end gap-3">
          <Button variant="ghost" size="md" onClick={() => props.onClose()}>
            取消
          </Button>
          <Button
            variant="primary"
            size="md"
            class="hover-lift"
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
