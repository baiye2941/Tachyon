import { Show, type JSX } from "solid-js";
import Button from "./Button";

interface EmptyStateAction {
  label: string;
  onClick: () => void;
  icon?: JSX.Element;
  ariaLabel?: string;
}

export interface EmptyStateProps {
  icon: JSX.Element;
  title: string | JSX.Element;
  description?: string | JSX.Element;
  /** 使用品牌强调色图标(不透明度更高),默认 false */
  brand?: boolean;
  action?: EmptyStateAction;
  /** 高亮操作按钮(首次使用 onboarding) */
  actionHighlight?: boolean;
  children?: JSX.Element;
  class?: string;
  /** 紧凑模式,用于命令面板等较小容器 */
  compact?: boolean;
}

/**
 * 统一空状态组件。
 *
 * 提供一致的图标、标题、可选副标题与操作按钮,消除各面板中临时写的
 * `<div>暂无...</div>` 等不一致占位 UI。
 */
export default function EmptyState(props: EmptyStateProps) {
  return (
    <div
      class="empty-state"
      classList={{
        "empty-state--compact": props.compact,
        ...(props.class ? { [props.class]: true } : {}),
      }}
    >
      <div
        class="empty-state-icon"
        classList={{ "empty-state-icon--brand": props.brand }}
      >
        {props.icon}
      </div>
      <div class="empty-state-body">
        <p class="empty-state-title">{props.title}</p>
        <Show when={props.description}>
          <p class="empty-state-desc">{props.description}</p>
        </Show>
        <Show when={props.action}>
          {(action) => (
            <Button
              variant="primary"
              size={props.compact ? "md" : "lg"}
              aria-label={action().ariaLabel ?? action().label}
              onClick={action().onClick}
              data-highlight={props.actionHighlight}
            >
              {action().icon}
              <span>{action().label}</span>
            </Button>
          )}
        </Show>
      </div>
      {props.children}
    </div>
  );
}
