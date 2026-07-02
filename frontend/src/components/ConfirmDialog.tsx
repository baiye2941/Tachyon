import { createEffect, createSignal, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { CloseIcon, WarningCircleIcon } from "./icons";
import { useFocusTrap } from "../hooks/useFocusTrap";
import { tr } from "../i18n";
import Button from "../shared/ui/Button";

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

  const [deleteLocalFile, setDeleteLocalFile] = createSignal(false);

  createEffect(() => {
    if (props.open) {
      setDeleteLocalFile(props.deleteLocalFileDefault ?? false);
    }
  });

  useFocusTrap({
    active: () => props.open,
    container: () => dialogRef,
    onEscape: () => props.onCancel(),
  });

  return (
    <Portal mount={document.body}>
      <Show when={props.open}>
        {/* 遮罩层 */}
        <div
          data-testid="dialog-overlay"
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
          role="alertdialog"
          aria-modal="true"
          aria-labelledby="confirm-dialog-title"
          aria-describedby="confirm-dialog-desc"
          class="fixed z-[310] flex flex-col"
          style={{
            top: "50%",
            left: "50%",
            transform: "translate(-50%, -50%)",
            width: "min(400px, calc(100vw - 32px))",
            /* 去 AI 味:实色背景 + 边框分层,移除装饰性顶部高光渐变 */
            background: "var(--color-bg-elevated)",
            "border-radius": "14px",
            border: "1px solid var(--color-border-default)",
            "box-shadow": "var(--shadow-xl)",
            padding: "24px",
            animation: "fadeIn 150ms ease forwards",
          }}
          onClick={(e) => e.stopPropagation()}
        >
          {/* 标题行 */}
          <div
            class="flex items-start justify-between"
            style={{ "margin-bottom": "12px", gap: "12px" }}
          >
            <div class="flex items-start" style={{ gap: "12px", "min-width": "0", "flex": "1" }}>
              <Show when={props.tone === "danger"}>
                <div
                  class="flex items-center justify-center flex-shrink-0"
                  style={{
                    width: "32px",
                    height: "32px",
                    "border-radius": "8px",
                    background: "var(--color-danger-soft)",
                    color: "var(--color-error)",
                  }}
                >
                  <WarningCircleIcon />
                </div>
              </Show>
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
            </div>
            <Button
              variant="ghost"
              shape="icon-sm"
              aria-label={tr("confirmDialog.aria.close")}
              onClick={() => props.onCancel()}
            >
              <CloseIcon />
            </Button>
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
            <Button
              variant="secondary"
              size="md"
              onClick={() => props.onCancel()}
              disabled={props.loading}
            >
              {props.cancelLabel ?? tr("common.cancel")}
            </Button>
            <Button
              variant={props.tone === "danger" ? "danger" : "primary"}
              size="md"
              data-autofocus
              loading={props.loading}
              onClick={() => props.onConfirm({ deleteLocalFile: deleteLocalFile() })}
              disabled={props.loading}
            >
              {props.loading ? tr("common.processing") : (props.confirmLabel ?? tr("common.confirm"))}
            </Button>
          </div>
        </div>
      </Show>
    </Portal>
  );
}
