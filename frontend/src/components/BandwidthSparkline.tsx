import {
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  Show,
} from "solid-js";
import type { TaskInfo } from "../types";
import { getTaskHistory } from "../stores/taskSpeedHistory";
import { useIsSmallScreen } from "../hooks/useMediaQuery";
import { tr } from "../i18n";
import { ChevronDownIcon } from "./icons";
import Sparkline from "./Sparkline";

interface BandwidthSparklineProps {
  taskId: string;
  status?: TaskInfo["status"];
}

const MAX_POINTS = 60;
const POLL_INTERVAL_MS = 1000;
const SPARKLINE_WIDTH = 240;
const SPARKLINE_HEIGHT = 40;

/**
 * Activity Metrics 区 mini 带宽历史曲线。
 *
 * - 从 taskSpeedHistory 读取最近 60 个采样点
 * - 仅下载态轮询刷新,其他状态静态显示
 * - 小屏默认折叠,用户可手动展开/收起
 */
export default function BandwidthSparkline(props: BandwidthSparklineProps) {
  const isSmall = useIsSmallScreen();
  const [tick, setTick] = createSignal(0);
  const [collapsed, setCollapsed] = createSignal(false);
  let timerId: number | undefined;

  // 小屏默认折叠;切换到大屏时不强制展开,保留用户选择
  createEffect(() => {
    if (isSmall()) {
      setCollapsed(true);
    }
  });

  const startTimer = () => {
    if (timerId !== undefined) return;
    const loop = () => {
      setTick((t) => t + 1);
      timerId = window.setTimeout(loop, POLL_INTERVAL_MS);
    };
    timerId = window.setTimeout(loop, POLL_INTERVAL_MS);
  };

  const stopTimer = () => {
    if (timerId !== undefined) {
      clearTimeout(timerId);
      timerId = undefined;
    }
  };

  createEffect(() => {
    if (props.status === "downloading") {
      startTimer();
    } else {
      stopTimer();
    }
  });

  onCleanup(() => {
    stopTimer();
  });

  const data = createMemo(() => {
    void tick();
    return getTaskHistory(props.taskId).slice(-MAX_POINTS);
  });

  const hasData = createMemo(() => data().length >= 2);

  const chevronClass = () =>
    `bandwidth-sparkline-chevron${
      !collapsed() ? " bandwidth-sparkline-chevron--open" : ""
    }`;

  return (
    <div
      class="bandwidth-sparkline"
      classList={{ "bandwidth-sparkline--collapsed": collapsed() }}
    >
      <button
        type="button"
        class="bandwidth-sparkline-toggle"
        onClick={() => setCollapsed((v) => !v)}
        aria-expanded={!collapsed()}
        aria-label={
          collapsed()
            ? tr("bandwidthSparkline.expand")
            : tr("bandwidthSparkline.collapse")
        }
      >
        <span class="section-label">{tr("bandwidthSparkline.title")}</span>
        <ChevronDownIcon class={chevronClass()} />
      </button>

      <Show when={!collapsed()}>
        <div class="bandwidth-sparkline-body">
          <Show
            when={hasData()}
            fallback={
              <div class="bandwidth-sparkline-empty">
                <span>{tr("bandwidthSparkline.waiting")}</span>
              </div>
            }
          >
            <Sparkline
              data={data()}
              width={SPARKLINE_WIDTH}
              height={SPARKLINE_HEIGHT}
            />
          </Show>
        </div>
      </Show>
    </div>
  );
}
