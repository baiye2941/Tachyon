export interface SliderItemProps {
  label: string;
  value: number;
  min: number;
  max: number;
  step?: number;
  onChange: (v: number) => void;
  displayValue: string;
}

export default function SliderItem(props: SliderItemProps) {
  // step<1 视为浮点滑块(如 EWMA alpha),用 parseFloat 解析;否则整数滑块用 parseInt
  // 注意:isFloat 在 onInput 内计算,避免在组件体中读取 props.step 触发非追踪访问告警
  return (
    <div>
      <div
        class="flex items-center justify-between"
        style={{ "margin-bottom": "8px" }}
      >
        <span
          style={{ "font-size": "13px", color: "var(--color-text-secondary)" }}
        >
          {props.label}
        </span>
        <span
          class="mono"
          style={{ "font-size": "13px", color: "var(--color-text-title)" }}
        >
          {props.displayValue}
        </span>
      </div>
      <input
        type="range"
        aria-label={props.label}
        min={props.min}
        max={props.max}
        step={props.step ?? 1}
        value={props.value}
        onInput={(e) => {
          // 在事件处理内读取 props.step,保证响应式追踪正确
          const isFloat = props.step !== undefined && props.step < 1;
          props.onChange(
            isFloat
              ? parseFloat(e.currentTarget.value)
              : parseInt(e.currentTarget.value),
          );
        }}
        style={{ width: "100%" }}
      />
    </div>
  );
}
