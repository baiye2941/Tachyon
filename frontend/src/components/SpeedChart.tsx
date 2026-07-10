import {
  createMemo,
  onCleanup,
  createSignal,
  createEffect,
  For,
  Show,
} from "solid-js";
import type { TaskInfo } from "../types";
import { formatSpeed } from "../utils/format";
import { getTaskHistory } from "../stores/taskSpeedHistory";
import { tr } from "../i18n";
import { ChartBarIcon } from "./icons";
import { useReducedMotion } from "../hooks/useReducedMotion";

interface SpeedChartProps {
  task: TaskInfo;
}

const MAX_POINTS = 60;
const GRID_LINES = 4;
const VIEWBOX_WIDTH = 320;
const VIEWBOX_HEIGHT = 120;
const PADDING = 8;

export default function SpeedChart(props: SpeedChartProps) {
  const reducedMotion = useReducedMotion();
  const [tick, setTick] = createSignal(0);
  let timerId: number | undefined;

  const startTimer = () => {
    if (timerId !== undefined) return;
    const loop = () => {
      setTick((t) => t + 1);
      timerId = window.setTimeout(loop, 1000);
    };
    timerId = window.setTimeout(loop, 1000);
  };

  const stopTimer = () => {
    if (timerId !== undefined) {
      clearTimeout(timerId);
      timerId = undefined;
    }
  };

  // 只在下载态才更新图表;完成/暂停/失败后停止轮询
  createEffect(() => {
    if (props.task.status === "downloading") {
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
    return getTaskHistory(props.task.id);
  });

  const hasData = createMemo(() => data().length > 0);

  const yMax = createMemo(() => {
    const history = data();
    const sample = history.length > 0 ? history : [props.task.speed];
    return Math.max(...sample, 1);
  });

  const pathD = createMemo(() => {
    const history = data();
    const sample = history.length > 0 ? history : [props.task.speed];
    const points =
      sample.length > MAX_POINTS ? sample.slice(-MAX_POINTS) : sample;
    const maxVal = yMax();
    const width = VIEWBOX_WIDTH;
    const height = VIEWBOX_HEIGHT;

    const coords = points.map((val, i) => {
      const x =
        points.length > 1 ? (i / (points.length - 1)) * width : width / 2;
      const y = height - PADDING - (val / maxVal) * (height - PADDING * 2);
      return [x, y] as const;
    });

    if (coords.length < 2) return { line: "", area: "", coords };

    const first = coords[0]!;
    let line = `M ${first[0]} ${first[1]}`;
    for (let i = 1; i < coords.length; i++) {
      const prev = coords[i - 1]!;
      const curr = coords[i]!;
      const cpx1 = prev[0] + (curr[0] - prev[0]) * 0.5;
      const cpx2 = prev[0] + (curr[0] - prev[0]) * 0.5;
      line += ` C ${cpx1} ${prev[1]}, ${cpx2} ${curr[1]}, ${curr[0]} ${curr[1]}`;
    }

    const area = `${line} L ${width} ${height} L 0 ${height} Z`;
    return { line, area, coords };
  });

  const stats = createMemo(() => {
    const samples = data().length > 0 ? data() : [props.task.speed];
    const peak = samples.length > 0 ? Math.max(...samples) : 0;
    const avg =
      samples.length > 0
        ? samples.reduce((a, b) => a + b, 0) / samples.length
        : 0;
    return { peak, avg };
  });

  const lastPoint = createMemo(() => {
    const coords = pathD().coords;
    return coords.length > 0 ? coords[coords.length - 1] : null;
  });

  const gridYs = Array.from(
    { length: GRID_LINES },
    (_, i) => ((i + 1) * VIEWBOX_HEIGHT) / (GRID_LINES + 1),
  );

  const yLabels = createMemo(() => {
    const maxVal = yMax();
    return [formatSpeed(maxVal), formatSpeed(maxVal * 0.5), formatSpeed(0)];
  });

  const xLabels = createMemo(() => {
    const totalSeconds = Math.max(0, data().length - 1);
    return {
      left: tr("speedChart.ago", { seconds: String(totalSeconds) }),
      middle: tr("speedChart.ago", {
        seconds: String(Math.round(totalSeconds / 2)),
      }),
      right: tr("speedChart.now"),
    };
  });

  return (
    <div class="glass speed-chart">
      <div class="speed-chart-header">
        <div class="speed-chart-title">
          <span class="speed-chart-live-dot" />
          <span class="section-label">{tr("speedChart.title")}</span>
        </div>
        <Show when={hasData()}>
          <span
            class="mono speed-chart-live"
            aria-label={tr("speedChart.live")}
          >
            {formatSpeed(props.task.speed)}
          </span>
        </Show>
      </div>

      <div class="speed-chart-body">
        <div class="speed-chart-y-axis" aria-hidden="true">
          <For each={yLabels()}>
            {(label) => <span class="speed-chart-y-label">{label}</span>}
          </For>
        </div>

        <div class="speed-chart-canvas">
          <svg
            class="speed-chart-svg"
            width="100%"
            height="100%"
            viewBox={`0 0 ${VIEWBOX_WIDTH} ${VIEWBOX_HEIGHT}`}
            preserveAspectRatio="none"
          >
            <defs>
              <linearGradient
                id="speed-area-gradient"
                x1="0"
                y1="0"
                x2="0"
                y2="1"
              >
                <stop offset="0%" stop-color="var(--color-speed-soft)" />
                <stop offset="60%" stop-color="var(--color-speed-soft)" />
                <stop offset="100%" stop-color="transparent" />
              </linearGradient>
              <linearGradient
                id="speed-line-gradient"
                x1="0"
                y1="0"
                x2="1"
                y2="0"
              >
                <stop offset="0%" stop-color="var(--color-accent-primary)" />
                <stop offset="100%" stop-color="var(--color-speed-active)" />
              </linearGradient>
            </defs>

            <For each={gridYs}>
              {(y) => (
                <line
                  x1="0"
                  y1={y}
                  x2={VIEWBOX_WIDTH}
                  y2={y}
                  class="speed-chart-grid"
                />
              )}
            </For>

            <Show when={hasData()}>
              <path
                d={pathD().area}
                fill="url(#speed-area-gradient)"
                stroke="none"
              />
              <path
                d={pathD().line}
                fill="none"
                stroke="url(#speed-line-gradient)"
                stroke-width="3"
                stroke-linecap="round"
                stroke-linejoin="round"
                class="speed-chart-line"
              />

              <Show when={lastPoint()}>
                {(pt) => (
                  <>
                    <Show when={!reducedMotion()}>
                      <circle
                        cx={pt()[0]}
                        cy={pt()[1]}
                        r="7"
                        fill="var(--color-speed-active)"
                        class="speed-chart-pulse-ring"
                      />
                    </Show>
                    <circle
                      cx={pt()[0]}
                      cy={pt()[1]}
                      r="4"
                      fill="var(--color-speed-active)"
                      stroke="var(--color-bg-secondary)"
                      stroke-width="2"
                    />
                  </>
                )}
              </Show>
            </Show>
          </svg>

          <Show when={!hasData()}>
            <div class="speed-chart-empty-overlay">
              <ChartBarIcon size={28} />
              <span>{tr("speedChart.waiting")}</span>
            </div>
          </Show>
        </div>
      </div>

      <div class="speed-chart-x-axis" aria-hidden="true">
        <span>{xLabels().left}</span>
        <span>{xLabels().middle}</span>
        <span>{xLabels().right}</span>
      </div>

      <div class="speed-chart-footer">
        <div class="speed-chart-stat">
          <span class="speed-chart-stat-dot speed-chart-stat-dot--peak" />
          <span class="speed-chart-stat-label">{tr("speedChart.peak")}</span>
          <span class="mono speed-chart-stat-value speed-chart-stat-value--peak">
            {formatSpeed(stats().peak)}
          </span>
        </div>
        <div class="speed-chart-stat">
          <span class="speed-chart-stat-dot" />
          <span class="speed-chart-stat-label">{tr("speedChart.average")}</span>
          <span class="mono speed-chart-stat-value">
            {formatSpeed(stats().avg)}
          </span>
        </div>
      </div>
    </div>
  );
}
