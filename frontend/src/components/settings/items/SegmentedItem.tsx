import { For } from "solid-js";

export interface SegmentedItemProps<T extends string> {
  label: string;
  value: T;
  options: { value: T; label: string }[];
  onChange: (v: T) => void;
}

/** 分段选择项(多选一,如 HF 源模式) */
export default function SegmentedItem<T extends string>(
  props: SegmentedItemProps<T>,
) {
  return (
    <div>
      <div style={{ "margin-bottom": "8px" }}>
        <span
          style={{ "font-size": "13px", color: "var(--color-text-title)" }}
        >
          {props.label}
        </span>
      </div>
      <div
        class="flex"
        style={{
          "border-radius": "8px",
          background: "var(--graphite-2)",
          padding: "2px",
          gap: "2px",
        }}
      >
        <For each={props.options}>
          {(opt) => (
            <button
              style={{
                flex: "1",
                padding: "6px 8px",
                "font-size": "12px",
                "border-radius": "6px",
                border: "none",
                cursor: "pointer",
                background:
                  props.value === opt.value
                    ? "var(--color-bg-primary)"
                    : "transparent",
                color:
                  props.value === opt.value
                    ? "var(--color-text-title)"
                    : "var(--color-text-secondary)",
                "font-weight": props.value === opt.value ? 600 : 400,
                transition: "all 150ms ease",
              }}
              onClick={() => props.onChange(opt.value)}
            >
              {opt.label}
            </button>
          )}
        </For>
      </div>
    </div>
  );
}
