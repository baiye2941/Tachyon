import { createEffect, createSignal, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { CloseIcon } from "./icons";
import { useFocusTrap } from "../hooks/useFocusTrap";
import { tr } from "../i18n";

interface ConfirmDialogProps {
  /** 是否显示对话框 */
  open: boolean;
  /** 对话框标题 */
  title: string;
  /** 对话框描述信息 */
  message: string;
  /** 确认按钮文本，默认"确认" */
  confirmLabel?: string;
  /** 取消按钮文本，默认"取消" */
  cancelLabel?: string;
  /** 确认按钮调性:danger 用于删除等破坏性操作(红色按钮),默认 primary */
  tone?: "primary" | "danger";
  /** 确认操作是否正在执行中 */
  loading?: boolean;
  /** 是否显示“同时删除本地文件”复选项 */
  showDeleteLocalFileOption?: boolean;
  /** 复选项文案 */
  deleteLocalFileLabel?: string;
  /** 复选项说明 */
  deleteLocalFileDescription?: string;
  /** 复选项默认值 */
  deleteLocalFileDefault?: boolean;
  /** 确认回调 */
  onConfirm: (options: { deleteLocalFile: boolean }) => void;
  /** 取消/关闭回调 */
  onCancel: () => void;
}

/**
 * 可复用的确认对话框组件(P1-11)
 *
 * 替代 window.confirm，提供:
 * - 完整的无障碍支持: role="dialog", aria-modal, 焦点陷阱, Esc 关闭
 * - 与应用 glass morphism 主题一致的视觉风格
 * - 加载状态支持
 */
export default function ConfirmDialog(props: ConfirmDialogProps) {
  let dialogRef: HTMLDivElement | undefined;
  let confirmBtnRef: HTMLButtonElement | undefined;

  const [deleteLocalFile, setDeleteLocalFile] = createSignal(false);

  createEffect(() => {
    if (props.open) {
      setDeleteLocalFile(props.deleteLocalFileDefault ?? false);
    }
  });

  useFocusTrap({
    active: () => props.open,
    container: dialogRef,
    onEscape: () => props.onCancel(),
  });

  return (
    <Portal mount={document.body}>
      <Show when={props.open}>
        {/* 遮罩层 */}
        <div
          class="fixed inset-0 z-[300]"
          style={{
            background: "var(--color-overlay-scrim)",
            "backdrop-filter": "blur(4px)",
            animation: "fadeIn 150ms ease forwards",
          }}
          onClick={() => props.onCancel()}
        />

        {/* 对话框 */}
        <div
          ref={dialogRef}
          role="dialog"
          aria-modal="true"
          aria-labelledby="confirm-dialog-title"
          aria-describedby="confirm-dialog-desc"
          class="fixed z-[310] flex flex-col"
          style={{
            top: "50%",
            left: "50%",
            transform: "translate(-50%, -50%)",
            width: "min(400px, calc(100vw - 32px))",
            /* 质感:实色 + 顶部极淡向下渐变,模拟环境光从上方照射 */
            background:
              "linear-gradient(180deg, rgba(255,255,255,0.022) 0%, transparent 96px), var(--color-bg-elevated)",
            "border-radius": "14px",
            border: "1px solid var(--color-border-default)",
            "box-shadow":
              "var(--shadow-xl), inset 0 1px 0 rgba(255, 255, 255, 0.06)",
            padding: "24px",
            animation: "fadeIn 150ms ease forwards",
          }}
          onClick={(e) => e.stopPropagation()}
        >
          {/* 标题行 */}
          <div
            class="flex items-start justify-between"
            style={{ "margin-bottom": "12px" }}
          >
            <h3
              id="confirm-dialog-title"
              style={{
                "font-size": "15px",
                "font-weight": 600,
                color: "var(--color-text-title)",
                "line-height": "1.4",
              }}
            >
              {props.title}
            </h3>
            <button
              aria-label={tr("confirmDialog.aria.close")}
              style={{
                background: "transparent",
                border: "none",
                cursor: "pointer",
                color: "var(--color-text-tertiary)",
                padding: "2px",
                "flex-shrink": 0,
                "margin-left": "8px",
              }}
              onClick={() => props.onCancel()}
            >
              <CloseIcon />
            </button>
          </div>

          {/* 描述 */}
          <p
            id="confirm-dialog-desc"
            style={{
              "font-size": "13px",
              color: "var(--color-text-secondary)",
              "line-height": "1.5",
              "margin-bottom": "20px",
              "word-break": "break-word",
            }}
          >
            {props.message}
          </p>

          {/* 删除本地文件选项 */}
          <Show when={props.showDeleteLocalFileOption}>
            <label
              class="flex items-start gap-3"
              style={{
                padding: "12px",
                "border-radius": "10px",
                border: "1px solid var(--color-border-subtle)",
                background: "var(--color-bg-secondary)",
                "box-shadow": "inset 0 1px 0 var(--color-bg-hover)",
                "margin-bottom": "20px",
                cursor: props.loading ? "not-allowed" : "pointer",
              }}
            >
              <input
                type="checkbox"
                checked={deleteLocalFile()}
                disabled={props.loading}
                onChange={(e) => setDeleteLocalFile(e.currentTarget.checked)}
                style={{
                  width: "16px",
                  height: "16px",
                  "margin-top": "2px",
                  "accent-color": "var(--color-error)",
                  cursor: props.loading ? "not-allowed" : "pointer",
                }}
              />
              <span class="flex flex-col" style={{ gap: "3px" }}>
                <span
                  style={{
                    "font-size": "13px",
                    "font-weight": 600,
                    color: "var(--color-text-title)",
                    "line-height": "1.35",
                  }}
                >
                  {props.deleteLocalFileLabel ?? tr("confirm.deleteLocalFile.label")}
                </span>
                <span
                  style={{
                    "font-size": "12px",
                    color: "var(--color-text-tertiary)",
                    "line-height": "1.45",
                  }}
                >
                  {props.deleteLocalFileDescription ?? tr("confirm.deleteLocalFile.description")}
                </span>
              </span>
            </label>
          </Show>

          {/* 按钮行 */}
          <div class="flex items-center justify-end gap-2">
            <button
              style={{
                padding: "6px 16px",
                "font-size": "13px",
                "border-radius": "6px",
                background: "var(--graphite-2)",
                color: "var(--color-text-secondary)",
                border: "none",
                cursor: "pointer",
                transition: "background 150ms ease",
              }}
              onClick={() => props.onCancel()}
              disabled={props.loading}
            >
              {props.cancelLabel ?? tr("common.cancel")}
            </button>
            <button
              ref={confirmBtnRef}
              data-autofocus
              style={{
                padding: "6px 16px",
                "font-size": "13px",
                "font-weight": 500,
                "border-radius": "6px",
                // danger 调性:删除等破坏性操作用红色警示(primary 默认品牌紫)
                background:
                  props.tone === "danger"
                    ? "var(--color-error)"
                    : "var(--color-accent-primary)",
                color: "var(--color-text-inverse)",
                border: "none",
                cursor: props.loading ? "wait" : "pointer",
                opacity: props.loading ? 0.7 : 1,
                transition: "opacity 150ms ease, background 150ms ease",
              }}
              onClick={() => props.onConfirm({ deleteLocalFile: deleteLocalFile() })}
              disabled={props.loading}
            >
              {props.loading ? tr("common.processing") : (props.confirmLabel ?? tr("common.confirm"))}
            </button>
          </div>
        </div>
      </Show>
    </Portal>
  );
}
