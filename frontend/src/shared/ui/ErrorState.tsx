import { Show, type JSX } from "solid-js";
import { WarningCircleIcon, RefreshIcon } from "../../components/icons";
import Button from "./Button";
import { tr } from "../../i18n";

export interface ErrorStateProps {
  title?: string | JSX.Element;
  message?: string | JSX.Element;
  detail?: string | JSX.Element;
  onRetry?: () => void;
  retryLabel?: string;
  class?: string;
  /** 紧凑模式,用于面板内错误状态 */
  compact?: boolean;
}

/**
 * 统一错误状态组件。
 *
 * 统一各面板的加载失败占位:错误图标 + 标题 + 可选详情 + 重试按钮。
 */
export default function ErrorState(props: ErrorStateProps) {
  const title = () => props.title ?? tr("common.appError");
  const retryLabel = () => props.retryLabel ?? tr("common.retry");

  return (
    <div
      class="error-state"
      classList={{
        "error-state--compact": props.compact,
        ...(props.class ? { [props.class]: true } : {}),
      }}
      role="alert"
      aria-live="assertive"
    >
      <div class="error-state-icon">
        <WarningCircleIcon />
      </div>
      <div class="error-state-body">
        <p class="error-state-title">{title()}</p>
        <Show when={props.message}>
          <p class="error-state-message">{props.message}</p>
        </Show>
        <Show when={props.detail}>
          <p class="error-state-detail">{props.detail}</p>
        </Show>
        <Show when={props.onRetry}>
          {(onRetry) => (
            <Button
              variant="primary"
              size={props.compact ? "md" : "lg"}
              onClick={onRetry()}
            >
              <RefreshIcon />
              <span>{retryLabel()}</span>
            </Button>
          )}
        </Show>
      </div>
    </div>
  );
}
