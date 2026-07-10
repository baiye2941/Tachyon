import { Show } from "solid-js";
import { tr } from "../../../i18n";

export interface ToggleItemProps {
  label: string;
  value: boolean;
  onChange: (v: boolean) => void;
  /** 副标记:"restart"=需重启生效(如 SOCKS5 禁 DHT 等需重建 Session 的项) */
  badge?: "restart";
}

export default function ToggleItem(props: ToggleItemProps) {
  const badgeText = () =>
    props.badge === "restart" ? tr("settings.magnet.restartRequired") : null;
  return (
    <div class="flex items-center justify-between">
      <div class="flex items-center" style={{ gap: "8px" }}>
        <span style={{ "font-size": "13px", color: "var(--color-text-title)" }}>
          {props.label}
        </span>
        <Show when={badgeText()}>
          <span
            style={{
              "font-size": "11px",
              color: "var(--color-text-tertiary)",
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
      <button
        class="relative"
        style={{
          width: "40px",
          height: "22px",
          "border-radius": "11px",
          background: props.value
            ? "var(--color-accent-primary)"
            : "var(--graphite-2)",
          border: "none",
          cursor: "pointer",
          transition: "background 200ms ease",
        }}
        onClick={() => props.onChange(!props.value)}
      >
        <div
          style={{
            position: "absolute",
            width: "18px",
            height: "18px",
            "border-radius": "50%",
            background: "white",
            top: "2px",
            left: "2px",
            transform: props.value ? "translateX(18px)" : "translateX(0)",
            transition: "transform 200ms cubic-bezier(0.32, 0.72, 0, 1)",
          }}
        />
      </button>
    </div>
  );
}
