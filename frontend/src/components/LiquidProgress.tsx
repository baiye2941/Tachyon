import { Show, createMemo, type JSX } from "solid-js";
import type { DownloadStatus } from "../types";
import { CheckIcon, WarningCircleIcon } from "./icons";

export type ProgressSize = "sm" | "md" | "lg";

interface LiquidProgressProps {
  progress: number;
  status: DownloadStatus;
  size?: ProgressSize;
  /** 是否显示完成/失败 icon(仅 detail 模式) */
  showStateIcon?: boolean;
  /** 自定义轨道高度(px) */
  height?: number;
  /** 减少动画偏好由组件内部读取,但也可外部强制 */
  reducedMotion?: boolean;
  style?: JSX.CSSProperties;
  class?: string;
  "aria-label"?: string;
}

const SIZE_HEIGHT: Record<ProgressSize, number> = {
  sm: 3,
  md: 5,
  lg: 7,
};

const WAITING_STATUSES: DownloadStatus[] = [
  "pending",
  "connecting",
  "resuming",
  "verifying",
];

export default function LiquidProgress(props: LiquidProgressProps) {
  const height = createMemo(
    () => props.height ?? SIZE_HEIGHT[props.size ?? "md"],
  );
  const pct = createMemo(() => Math.max(0, Math.min(1, props.progress)) * 100);
  const hasProgress = createMemo(() => props.progress > 0);

  const isFailed = createMemo(() => props.status === "failed");
  const isCompleted = createMemo(() => props.status === "completed");
  const isDownloading = createMemo(() => props.status === "downloading");
  const isWaiting = createMemo(() => WAITING_STATUSES.includes(props.status));
  const isPaused = createMemo(() => props.status === "paused");

  const shouldAnimateWaiting = createMemo(
    () => isWaiting() && !hasProgress() && !props.reducedMotion,
  );

  return (
    <div
      class={props.class}
      style={{
        ...props.style,
        position: "relative",
        width: "100%",
      }}
      aria-label={props["aria-label"]}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(pct() * 10) / 10}
      role="progressbar"
    >
      {/* Track */}
      <div
        class="progress-track-inset overflow-hidden"
        classList={{
          "progress-track--waiting": shouldAnimateWaiting(),
        }}
        style={{
          height: `${height()}px`,
          "border-radius": "9999px",
        }}
      >
        {/* Fill */}
        <div
          class="absolute left-0 top-0 bottom-0"
          classList={{
            "progress-bar-active": isDownloading(),
            "progress-fill--downloading": isDownloading(),
            "progress-fill--waiting": isWaiting() && !isDownloading(),
            "progress-fill--paused": isPaused(),
            "progress-fill--completed": isCompleted(),
            "progress-fill--failed": isFailed(),
          }}
          style={{
            width: `${pct()}%`,
            "min-width": hasProgress() ? `${height()}px` : "0px",
            "border-radius": "9999px",
            transition:
              "width 320ms cubic-bezier(0.32, 0.72, 0, 1), background 300ms ease",
          }}
        />
      </div>

      {/* 完成/失败状态 icon(不遮挡进度条,悬浮在右侧) */}
      <Show when={props.showStateIcon && (isCompleted() || isFailed())}>
        <div
          class="progress-state-icon"
          classList={{
            "progress-state-icon--success": isCompleted(),
            "progress-state-icon--error": isFailed(),
          }}
          aria-hidden="true"
        >
          {isCompleted() ? <CheckIcon /> : <WarningCircleIcon />}
        </div>
      </Show>
    </div>
  );
}
