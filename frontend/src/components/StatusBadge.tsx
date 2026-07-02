import { Show } from "solid-js";
import { Dynamic } from "solid-js/web";
import type { DownloadStatus } from "../types";
import {
  DownloadSimpleIcon,
  CheckCircleIcon,
  WarningCircleIcon,
  PauseCircleIcon,
  ClockIcon,
  LightningIcon,
  PlayIcon,
  CancelIcon,
} from "./icons";
import { getStatusLabel } from "../utils/format";

interface StatusBadgeProps {
  status: DownloadStatus;
  showIcon?: boolean;
  size?: "sm" | "md";
  title?: string;
}

const STATUS_ICON: Partial<Record<DownloadStatus, typeof DownloadSimpleIcon>> =
  {
    downloading: DownloadSimpleIcon,
    completed: CheckCircleIcon,
    failed: WarningCircleIcon,
    paused: PauseCircleIcon,
    pending: ClockIcon,
    connecting: LightningIcon,
    resuming: PlayIcon,
    verifying: CheckCircleIcon,
    cancelled: CancelIcon,
  };

const ICON_SIZE = { sm: 10, md: 12 } as const;

export default function StatusBadge(props: StatusBadgeProps) {
  const sizeClass = () => `status-badge status-badge--${props.status}`;
  const iconSize = () => ICON_SIZE[props.size ?? "sm"];

  return (
    <span
      class={sizeClass()}
      title={props.title ?? getStatusLabel(props.status)}
    >
      <span
        class="status-badge-dot"
        classList={{
          [`status-badge-dot--${props.status}`]: true,
        }}
      />
      <Show when={props.showIcon}>
        <span class="status-badge-icon">
          <Dynamic component={STATUS_ICON[props.status]} size={iconSize()} />
        </span>
      </Show>
      {getStatusLabel(props.status)}
    </span>
  );
}
