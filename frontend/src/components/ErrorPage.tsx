import { createSignal, Show, type JSX } from "solid-js";
import { WarningCircleIcon, RefreshIcon } from "./icons";
import { tr } from "../i18n";
import Button from "../shared/ui/Button";

/**
 * 应用级错误页(ErrorBoundary fallback)。
 *
 * 任意组件抛出未捕获异常时,ErrorBoundary 渲染本页:
 * - 警示图标 + 标题 + 错误信息
 * - 可折叠堆栈(默认收起,避免长堆栈刷屏)
 * - 重试按钮(重载页面恢复初始状态)
 *
 * 视觉对齐 Tachyon 暗色材质:电青强调 + 错误红警示,无玻璃拟态。
 */
export function ErrorPage(props: { error: unknown }): JSX.Element {
  const [stackExpanded, setStackExpanded] = createSignal(false);

  // 响应式安全读取:在 getter 内访问 props.error(solid/reactivity 规则)
  const err = () => props.error;
  const message = () => {
    const e = err();
    return e instanceof Error ? e.message : String(e);
  };
  const stack = () => {
    const e = err();
    return e instanceof Error ? e.stack : undefined;
  };

  const handleRetry = () => {
    // 重载页面恢复初始状态(最可靠的重置方式)
    if (typeof window !== "undefined") {
      window.location.reload();
    }
  };

  return (
    <div
      class="flex items-center justify-center p-8"
      style={{
        "min-height": "100dvh",
        background: "var(--color-bg-primary)",
        color: "var(--color-text-primary)",
      }}
    >
      <div
        class="flex flex-col gap-5 max-w-lg w-full"
        style={{
          padding: "32px",
          "border-radius": "var(--radius-lg)",
          background: "var(--color-bg-elevated)",
          border: "1px solid var(--color-border-default)",
          "box-shadow": "var(--shadow-xl)",
        }}
      >
        {/* 警示图标 + 标题 */}
        <div class="flex items-start gap-4">
          <div
            class="flex items-center justify-center flex-shrink-0"
            style={{
              width: "40px",
              height: "40px",
              "border-radius": "10px",
              background: "var(--color-danger-soft)",
              color: "var(--color-error)",
            }}
          >
            <WarningCircleIcon />
          </div>
          <div class="flex flex-col gap-1 min-w-0">
            <h1
              style={{
                "font-size": "18px",
                "font-weight": 600,
                color: "var(--color-text-title)",
                "line-height": "1.3",
              }}
            >
              {tr("common.appError")}
            </h1>
            <p
              style={{
                "font-size": "13px",
                color: "var(--color-text-tertiary)",
                "line-height": "1.5",
              }}
            >
              {tr("common.appErrorHint")}
            </p>
          </div>
        </div>

        {/* 错误信息(主消息,可读) */}
        <div
          class="mono"
          style={{
            padding: "12px 14px",
            "border-radius": "var(--radius-md)",
            background: "var(--color-bg-void)",
            border: "1px solid var(--color-border-subtle)",
            "font-size": "13px",
            color: "var(--color-status-error)",
            "word-break": "break-word",
            "white-space": "pre-wrap",
            "line-height": "1.5",
          }}
        >
          {message()}
        </div>

        {/* 可折叠堆栈 */}
        <Show when={stack()}>
          <div class="flex flex-col gap-2">
            <button
              class="flex items-center gap-1.5 self-start"
              style={{
                "font-size": "12px",
                "font-weight": 500,
                color: "var(--color-text-tertiary)",
                background: "transparent",
                border: "none",
                cursor: "pointer",
                padding: "2px 0",
              }}
              onClick={() => setStackExpanded((v) => !v)}
              aria-expanded={stackExpanded()}
            >
              <span style={{ "font-family": "var(--font-mono)" }}>
                {stackExpanded() ? "▾" : "▸"}
              </span>
              {tr("common.errorStack")}
            </button>
            <Show when={stackExpanded()}>
              <pre
                class="mono"
                style={{
                  margin: "0",
                  padding: "12px 14px",
                  "border-radius": "var(--radius-md)",
                  background: "var(--color-bg-void)",
                  border: "1px solid var(--color-border-subtle)",
                  "font-size": "11px",
                  "line-height": "1.6",
                  color: "var(--color-text-secondary)",
                  "white-space": "pre-wrap",
                  "word-break": "break-word",
                  "overflow-wrap": "anywhere",
                  "max-height": "280px",
                  overflow: "auto",
                }}
              >
                {stack()}
              </pre>
            </Show>
          </div>
        </Show>

        {/* 操作:重试 */}
        <div class="flex justify-end gap-2" style={{ "margin-top": "4px" }}>
          <Button variant="primary" size="md" onClick={handleRetry}>
            <RefreshIcon />
            <span>{tr("common.retry")}</span>
          </Button>
        </div>
      </div>
    </div>
  );
}

export default ErrorPage;
