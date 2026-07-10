import { Show } from "solid-js";
import { tr } from "../../i18n";

export type SkeletonVariant = "panel" | "dialog" | "list" | "card";

interface SkeletonProps {
  /** 骨架屏视觉变体 */
  variant?: SkeletonVariant;
  /** 屏幕阅读器标签，默认使用 common.loading */
  label?: string;
  class?: string;
}

function Row(props: { width?: string; height?: string }) {
  return (
    <div
      class="skeleton__row"
      style={{
        width: props.width ?? "100%",
        height: props.height ?? "12px",
      }}
      aria-hidden="true"
    />
  );
}

/**
 * 统一骨架屏组件。
 *
 * 替代行内 `animate-pulse bg-white/5` fallback，提供:
 * - 结构化的面板/对话框/列表/卡片骨架
 * - `role="status"` + `aria-busy="true"` 无障碍声明
 * - 跟随设计 token 的实色背景，浅色主题下依然可见
 */
export default function Skeleton(props: SkeletonProps) {
  const label = () => props.label ?? tr("common.loading");
  const variant = () => props.variant ?? "card";

  return (
    <div
      class={`skeleton skeleton--${variant()} ${props.class ?? ""}`}
      role="status"
      aria-busy="true"
      aria-label={label()}
    >
      <span class="sr-only">{label()}</span>

      <Show when={variant() === "panel"}>
        <div class="skeleton__header">
          <Row width="40%" height="16px" />
        </div>
        <div class="skeleton__body">
          <Row width="70%" />
          <Row width="90%" />
          <Row width="60%" />
          <Row width="85%" />
          <Row width="50%" />
          <Row width="75%" />
        </div>
      </Show>

      <Show when={variant() === "dialog"}>
        <div class="skeleton__dialog-card">
          <div class="skeleton__dialog-header">
            <Row width="60%" height="16px" />
          </div>
          <div class="skeleton__dialog-body">
            <Row width="100%" />
            <Row width="90%" />
            <Row width="70%" />
          </div>
          <div class="skeleton__dialog-footer">
            <Row width="72px" height="32px" />
            <Row width="72px" height="32px" />
          </div>
        </div>
      </Show>

      <Show when={variant() === "list"}>
        <div class="skeleton__search">
          <Row width="100%" height="36px" />
        </div>
        <div class="skeleton__list">
          <Row width="95%" />
          <Row width="85%" />
          <Row width="90%" />
          <Row width="75%" />
          <Row width="88%" />
          <Row width="65%" />
          <Row width="80%" />
          <Row width="70%" />
        </div>
      </Show>

      <Show when={variant() === "card"}>
        <div class="skeleton__card">
          <Row width="60%" height="14px" />
          <Row width="100%" />
          <Row width="80%" />
          <Row width="50%" />
        </div>
      </Show>
    </div>
  );
}
