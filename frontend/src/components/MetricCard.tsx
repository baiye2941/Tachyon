import {
  createEffect,
  createSignal,
  onCleanup,
  Show,
  type JSX,
} from "solid-js";

interface MetricCardProps {
  label: string;
  value: string | number;
  highlight?: boolean;
  hint?: string;
  icon?: JSX.Element;
}

const PULSE_COOLDOWN_MS = 500;
const PULSE_DURATION_MS = 320;

/**
 * 详情页指标卡片。
 *
 * 数值默认静态显示,仅在值真正变化时触发一次短暂的柔和 pulse,
 * 既保留"数据在更新"的反馈,又避免持续翻页动画造成的视觉疲劳。
 */
export default function MetricCard(props: MetricCardProps) {
  const [pulse, setPulse] = createSignal(false);
  let prevValue: string | number | undefined;
  let lastPulseTime = 0;

  createEffect(() => {
    const current = props.value;
    if (prevValue !== undefined && prevValue !== current) {
      const now = Date.now();
      if (now - lastPulseTime >= PULSE_COOLDOWN_MS) {
        lastPulseTime = now;
        setPulse(true);
        const timer = setTimeout(() => setPulse(false), PULSE_DURATION_MS);
        onCleanup(() => clearTimeout(timer));
      }
    }
    prevValue = current;
  });

  return (
    <div class="metric-card" title={props.hint}>
      <div class="metric-card-header">
        <span class="metric-card-label">{props.label}</span>
        <Show when={props.icon}>
          <span class="metric-card-icon">{props.icon}</span>
        </Show>
      </div>
      <div
        class="metric-card-value mono"
        classList={{
          "metric-card-value--highlight": props.highlight,
          "metric-card-value--pulse": pulse(),
        }}
      >
        {props.value}
      </div>
    </div>
  );
}
