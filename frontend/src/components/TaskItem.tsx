import { createMemo, For, Show } from "solid-js";
import type { TaskInfo, ListDensity } from "../types";
import { CheckboxIcon } from "./icons";
import {
  formatSize,
  formatSpeed,
  getFileType,
  getStatusLabel,
} from "../utils/format";
import { tr } from "../i18n";
import LiquidProgress from "./LiquidProgress";
import StatusBadge from "./StatusBadge";

interface TaskItemProps {
  task: TaskInfo;
  isSelected: boolean;
  isMultiSelected: boolean;
  isMultiSelectMode: boolean;
  onClick: () => void;
  onContextMenu?: (e: MouseEvent, taskId: string) => void;
  density: ListDensity;
  searchQuery?: string;
  staggerIndex?: number;
}

/**
 * 搜索高亮文本组件。
 *
 * 用 String.split(regex) 单次分割(O(n))替代原先的 indexOf 循环(O(n×m)),
 * 大小写不敏感由正则 i 标志处理,无需预先 toLowerCase 整串。
 * 无 query 时返回 null,fallback 直接渲染原文,避免无谓的数组创建。
 *
 * 高亮用 <mark class="search-highlight"> 语义化标签,样式走 CSS token。
 */
function HighlightedText(props: { text: string; query: string }) {
  const segments = createMemo(() => {
    const query = props.query.trim();
    if (!query) return null; // null = 无高亮,直接渲染原文

    try {
      // 转义正则特殊字符,避免恶意输入触发 ReDoS
      const escaped = query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      const regex = new RegExp(`(${escaped})`, "gi");
      const result = props.text.split(regex);
      // split 带捕获组会保留分隔符:奇数下标 = 匹配项
      return result.length > 1 ? result : null;
    } catch {
      return null; // 非法正则回退
    }
  });

  return (
    <Show when={segments()} fallback={props.text}>
      {(segs) => (
        <For each={segs()}>
          {(seg, i) => {
            // eslint-disable-next-line solid/reactivity -- <For> 回调是 tracked scope,i() 安全
            const isMatch = i() % 2 === 1;
            return isMatch ? <mark class="search-highlight">{seg}</mark> : seg;
          }}
        </For>
      )}
    </Show>
  );
}

export default function TaskItem(props: TaskItemProps) {
  const fileInfo = createMemo(() => getFileType(props.task.fileName));
  const isCompact = createMemo(() => props.density === "compact");

  const handleKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      props.onClick();
    }
  };

  const ariaLabel = () => {
    const progress = (props.task.progress * 100).toFixed(1);
    const status = getStatusLabel(props.task.status);
    return tr("taskList.aria.taskItem", {
      name: props.task.fileName,
      progress,
      status,
    });
  };

  return (
    <div
      role="button"
      tabindex="0"
      aria-label={ariaLabel()}
      class="task-row cursor-pointer task-item-enter focus:outline-none focus-visible:focus-ring"
      classList={{
        "task-row--selected": props.isSelected && !props.isMultiSelected,
        "task-row--multi-selected": props.isMultiSelected,
        "task-row--compact": isCompact(),
      }}
      style={{
        position: "relative",
        "--stagger-index": props.staggerIndex ?? 0,
      }}
      onClick={() => props.onClick()}
      onKeyDown={handleKeyDown}
      onContextMenu={(e) => props.onContextMenu?.(e, props.task.id)}
    >
      <div class="flex items-center gap-3">
        <Show when={props.isMultiSelectMode}>
          <div
            class="task-checkbox flex items-center justify-center flex-shrink-0"
            role="checkbox"
            aria-checked={props.isMultiSelected}
            aria-label={tr("taskList.aria.selectTask", {
              name: props.task.fileName,
            })}
            style={{
              color: props.isMultiSelected
                ? "var(--color-accent-primary)"
                : "var(--color-text-tertiary)",
            }}
          >
            <CheckboxIcon checked={props.isMultiSelected} />
          </div>
        </Show>

        {/* 文件图标材质板(参考稿 file-icon:160deg 渐变 + 顶光 inset + drop-shadow)
            hue 由 --color-file-* token 驱动,图标本身已 duotone */}
        <div
          class="flex items-center justify-center flex-shrink-0 file-icon-plate task-file-icon"
          classList={{ "task-file-icon--compact": isCompact() }}
          style={{
            color: fileInfo().color,
            "--file-hue": fileInfo().color,
          }}
        >
          {(() => {
            const Icon = fileInfo().icon;
            return <Icon />;
          })()}
        </div>

        <div class="flex-1 min-w-0">
          <div class="flex items-center justify-between min-w-0">
            <div class="flex-1 min-w-0">
              <div
                class="truncate task-file-name"
                classList={{ "task-file-name--compact": isCompact() }}
              >
                <HighlightedText
                  text={props.task.fileName}
                  query={props.searchQuery || ""}
                />
              </div>
              {/* compact 模式隐藏元信息行,换取信息密度 */}
              <Show when={!isCompact()}>
                <div class="truncate task-file-meta">
                  {props.task.fileSize
                    ? formatSize(props.task.fileSize)
                    : tr("taskList.unknownSize")}
                  {" · "}
                  {props.task.url.split(":")[0]?.toUpperCase() ?? ""}
                  {props.task.speed > 0 &&
                    ` · ${formatSpeed(props.task.speed)}`}
                </div>
              </Show>
            </div>

            <div
              class="flex-shrink-0 task-col-progress"
              classList={{ "task-col-progress--compact": isCompact() }}
            >
              {(props.task.progress * 100).toFixed(1)}%
            </div>

            <div
              class="flex-shrink-0 task-col-speed"
              classList={{
                "task-col-speed--compact": isCompact(),
                "task-col-speed--active": props.task.status === "downloading",
              }}
            >
              {formatSpeed(props.task.speed)}
            </div>

            <div class="flex-shrink-0 flex justify-end task-col-status">
              <StatusBadge status={props.task.status} showIcon size="sm" />
            </div>
          </div>

          <LiquidProgress
            progress={props.task.progress}
            status={props.task.status}
            size="sm"
            class={`task-row-progress ${isCompact() ? "task-row-progress--compact" : ""}`}
            aria-label={ariaLabel()}
          />
        </div>
      </div>
    </div>
  );
}
