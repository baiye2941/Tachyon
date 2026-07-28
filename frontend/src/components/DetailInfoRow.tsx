import { createSignal, Show } from "solid-js";
import { CopyIcon, CheckIcon, ChevronDownIcon } from "./icons";
import { tr } from "../i18n";

/**
 * 详情面板元数据行(纯展示组件)。
 *
 * 从 DetailPanel.tsx 提取,零状态耦合:仅依赖 props + CSS 类 + tr()。
 * label/value 展示,可选复制按钮(复制后显示 CheckIcon,去 AI 味:用 SVG
 * 替代原先的 &#10003; 字符对勾)。
 * collapsible:长值(如下载链接)默认单行省略,chevron 展开/收起。
 */
export default function DetailInfoRow(props: {
  label: string;
  value: string;
  copyable?: boolean;
  copied?: boolean;
  onCopy?: () => void;
  collapsible?: boolean;
}) {
  const [expanded, setExpanded] = createSignal(false);
  const clamped = () => props.collapsible === true && !expanded();

  return (
    <div class="detail-info-row">
      <div class="min-w-0 flex-1 overflow-hidden">
        <div class="detail-info-label">{props.label}</div>
        <div
          class="detail-info-value"
          classList={{ "detail-info-value--clamped": clamped() }}
        >
          {props.value}
        </div>
      </div>
      <Show when={props.collapsible}>
        <button
          class="detail-disclosure-btn"
          style={{ "flex-shrink": 0 }}
          aria-expanded={expanded()}
          aria-label={
            expanded() ? tr("detail.url.collapse") : tr("detail.url.expand")
          }
          title={
            expanded() ? tr("detail.url.collapse") : tr("detail.url.expand")
          }
          onClick={() => setExpanded((v) => !v)}
        >
          <ChevronDownIcon
            class={`detail-disclosure-chevron${
              expanded() ? " detail-disclosure-chevron--open" : ""
            }`}
          />
        </button>
      </Show>
      <Show when={props.copyable}>
        <button
          class="icon-btn-sm"
          style={{ "flex-shrink": 0, width: "24px", height: "24px" }}
          onClick={() => props.onCopy?.()}
          aria-label={
            props.copied ? tr("detail.copied.aria") : tr("detail.copy.aria")
          }
          title={
            props.copied ? tr("detail.copied.aria") : tr("detail.copy.aria")
          }
        >
          {/* 去 AI 味:CheckIcon SVG 替代字符对勾 &#10003;,currentColor 跟随父级 */}
          <Show when={props.copied} fallback={<CopyIcon />}>
            <span style={{ color: "var(--color-accent-primary)", display: "flex" }}>
              <CheckIcon />
            </span>
          </Show>
        </button>
      </Show>
    </div>
  );
}
