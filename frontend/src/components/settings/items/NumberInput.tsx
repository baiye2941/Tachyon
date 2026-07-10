import { Show } from "solid-js";
import { tr } from "../../../i18n";

export interface NumberInputProps {
  label: string;
  value: number;
  min: number;
  max: number;
  unit?: string;
  /** 副标记类型:restart=需重启生效,newTask=对新任务生效 */
  badge?: "restart" | "newTask";
  hint?: string;
  onChange: (v: number) => void;
}

/**
 * 数值输入项(Task 9 Peer 优化)。
 *
 * 设计沿用 SliderItem / 下载限速的视觉模式:label + 副标记(badge)在上,
 * 数字输入框在下,可选 hint 文案。与 SliderItem 不同的是采用 type=number
 * 输入框(而非 range 滑块),因为 peer 超时等字段范围跨度大(1-3600),
 * 滑块精度不足且占横向空间过多。
 *
 * 副标记:
 *  - "restart"(需重启生效):peer 超时 / defer_writes / disable_dht_when_socks
 *  - "newTask"(对新任务生效):force_tracker_interval
 */
export default function NumberInput(props: NumberInputProps) {
  const badgeText = () => {
    if (props.badge === "restart") return tr("settings.magnet.restartRequired");
    if (props.badge === "newTask") return tr("settings.magnet.newTaskOnly");
    return null;
  };
  const badgeColor = () =>
    props.badge === "restart"
      ? "var(--color-text-tertiary)"
      : "var(--color-accent-secondary)";
  return (
    <div>
      <div
        class="flex items-center justify-between"
        style={{ "margin-bottom": "6px", gap: "8px" }}
      >
        <span
          style={{
            "font-size": "13px",
            color: "var(--color-text-title)",
          }}
        >
          {props.label}
        </span>
        <Show when={badgeText()}>
          <span
            style={{
              "font-size": "11px",
              color: badgeColor(),
              background: "var(--color-bg-secondary)",
              padding: "1px 6px",
              "border-radius": "4px",
              "white-space": "nowrap",
            }}
          >
            {badgeText()}
          </span>
        </Show>
      </div>
      <div class="flex items-center gap-2">
        <input
          type="number"
          aria-label={props.label}
          min={props.min}
          max={props.max}
          step={1}
          class="input"
          style={{ width: "120px", "font-size": "13px" }}
          value={props.value}
          onInput={(e) => {
            const raw = e.currentTarget.value.trim();
            if (raw === "") return;
            const n = parseInt(raw, 10);
            if (Number.isFinite(n)) {
              props.onChange(n);
            }
          }}
        />
        <Show when={props.unit}>
          <span
            class="mono"
            style={{
              "font-size": "11px",
              color: "var(--color-text-tertiary)",
              "white-space": "nowrap",
            }}
          >
            {props.unit}
          </span>
        </Show>
      </div>
      <Show when={props.hint}>
        <div
          style={{
            "font-size": "11px",
            color: "var(--color-text-tertiary)",
            "margin-top": "4px",
            "line-height": "1.5",
          }}
        >
          {props.hint}
        </div>
      </Show>
    </div>
  );
}
