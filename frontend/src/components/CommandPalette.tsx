import { createSignal, Show, For, createMemo, type JSX } from "solid-js";
import { Icon } from "../utils/icons";
import type { ViewName } from "../types";
import {
  COMMANDS,
  GROUP_LABEL_KEYS,
  type Command,
  type CommandContext,
  type CommandGroup,
} from "../commands/registry";
import { fuzzySearch } from "../utils/fuzzySearch";
import { tr, type MessageKey } from "../i18n";

interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
  onViewChange: (view: ViewName) => void;
  onNewDownload?: () => void;
  onPauseAll?: () => void;
  onResumeAll?: () => void;
  onToggleSidebar?: () => void;
  /** 任务搜索数据源(spec 8.6) */
  getTasks?: () => { id: string; fileName: string; url: string }[];
  /** 选中任务后打开详情 */
  onOpenTask?: (taskId: string) => void;
}

/** 高亮匹配字符:根据 matchedIndices 在 label 中包裹 <mark> */
function highlight(text: string, indices: number[]): JSX.Element {
  if (indices.length === 0) return text;
  const set = new Set(indices);
  const out: (string | { mark: string })[] = [];
  let buf = "";
  for (let i = 0; i < text.length; i++) {
    if (set.has(i)) {
      if (buf) {
        out.push(buf);
        buf = "";
      }
      out.push({ mark: text[i]! });
    } else {
      buf += text[i];
    }
  }
  if (buf) out.push(buf);
  return out.map((p) =>
    typeof p === "string" ? (
      p
    ) : (
      <mark
        style={{
          color: "var(--color-accent-secondary)",
          background: "transparent",
        }}
      >
        {p.mark}
      </mark>
    ),
  ) as unknown as JSX.Element;
}

export default function CommandPalette(props: CommandPaletteProps) {
  let inputRef: HTMLInputElement | undefined;
  let listRef: HTMLDivElement | undefined;
  const t = (key: MessageKey) => tr(key);
  const [query, setQuery] = createSignal("");
  const [activeIndex, setActiveIndex] = createSignal(0);

  // CommandContext:从 props 注入,registry 命令通过它执行(不捕获 props)。
  // 用 getter 惰性读取,保持响应式 + 满足 solid/reactivity 规则(props 在
  // 事件处理器内通过 getter 访问,而非组件体顶层立即求值)。
  const ctx: CommandContext = {
    get onViewChange() {
      return props.onViewChange;
    },
    get onClose() {
      return props.onClose;
    },
    get onNewDownload() {
      return props.onNewDownload;
    },
    get onPauseAll() {
      return props.onPauseAll;
    },
    get onResumeAll() {
      return props.onResumeAll;
    },
    get onToggleSidebar() {
      return props.onToggleSidebar;
    },
    get getTasks() {
      return props.getTasks;
    },
    get onOpenTask() {
      return props.onOpenTask;
    },
  };

  // fuzzy 搜索(子序列匹配 + 评分排序),替换原 includes。
  // 搜索文本用当前语言翻译后的 label/hint,保证用户输入中文可命中。
  // 任务搜索(spec 8.6):将匹配的任务包装为合成 Command(id 前缀 task-open:),
  // 归入 task 分组,选中后调用 onOpenTask 打开任务详情。
  const commandResults = createMemo(() =>
    fuzzySearch(
      COMMANDS,
      query(),
      (c) => `${t(c.labelKey)} ${c.hintKey ? t(c.hintKey) : ""}`,
    ).map((r) => ({
      item: r.item,
      score: r.score,
      matchedIndices: r.matchedIndices,
      taskFileName: undefined as string | undefined,
    })),
  );

  const taskResults = createMemo(() => {
    const tasks = ctx.getTasks?.() ?? [];
    if (tasks.length === 0) return [];
    return fuzzySearch(tasks, query(), (task) => `${task.fileName} ${task.url}`).map(
      (r) => {
        const task = r.item;
        const synthetic: Command = {
          id: `task-open:${task.id}`,
          labelKey: "command.task.openTask" as MessageKey,
          group: "task",
          icon: "list-bullet",
          run: (c) => {
            c.onOpenTask?.(task.id);
            c.onClose();
          },
        };
        return {
          item: synthetic,
          score: r.score,
          matchedIndices: r.matchedIndices,
          taskFileName: task.fileName,
        };
      },
    );
  });

  // 合并命令 + 任务结果,按 score 降序(分数越高越靠前)
  const results = createMemo(() => {
    const merged = [...commandResults(), ...taskResults()];
    merged.sort((a, b) => b.score - a.score);
    return merged;
  });

  // 按分组聚合(保持 fuzzy score 排序内的分组)
  // taskFileName:任务条目用文件名作为可搜索 label(合成 Command 的 labelKey 仅作占位)
  const grouped = createMemo(() => {
    const items = results();
    const byGroup: Record<
      CommandGroup,
      { cmd: Command; indices: number[]; taskFileName?: string }[]
    > = {
      navigation: [],
      action: [],
      task: [],
    };
    for (const r of items) {
      byGroup[r.item.group].push({
        cmd: r.item,
        indices: r.matchedIndices,
        taskFileName: r.taskFileName,
      });
    }
    return byGroup;
  });

  const groupOrder: CommandGroup[] = ["navigation", "task", "action"];

  // 执行当前选中项(按扁平 results 的 activeIndex)
  function executeActive() {
    const items = results();
    const idx = activeIndex();
    if (idx >= 0 && idx < items.length) {
      items[idx]?.item.run(ctx);
    }
  }

  function scrollActiveIntoView() {
    const el = listRef?.querySelector(`[data-cmd-index="${activeIndex()}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (!props.open) return;
    switch (e.key) {
      case "Escape":
        e.preventDefault();
        props.onClose();
        break;
      case "ArrowDown":
        e.preventDefault();
        setActiveIndex((i) => {
          const total = results().length;
          return total === 0 ? 0 : (i + 1) % total;
        });
        scrollActiveIntoView();
        break;
      case "ArrowUp":
        e.preventDefault();
        setActiveIndex((i) => {
          const total = results().length;
          return total === 0 ? 0 : (i - 1 + total) % total;
        });
        scrollActiveIntoView();
        break;
      case "Enter":
        e.preventDefault();
        executeActive();
        break;
    }
  }

  function handleOverlayClick(e: MouseEvent) {
    if (e.target === e.currentTarget) {
      props.onClose();
    }
  }

  return (
    <Show when={props.open}>
      <div
        class="fixed inset-0 z-[100] flex items-start justify-center pt-[15vh]"
        role="dialog"
        aria-modal="true"
        aria-label={t("commandPalette.aria")}
        style={{
          background: "var(--color-overlay-scrim)",
          "backdrop-filter": "blur(4px)",
        }}
        onClick={handleOverlayClick}
        onKeyDown={handleKeyDown}
        ref={() => {
          requestAnimationFrame(() => inputRef?.focus());
        }}
      >
        <div
          class="panel-surface rounded-lg w-full max-w-lg flex flex-col max-h-[60vh]"
          style={{
            "box-shadow": "var(--shadow-xl)",
            animation: "card-fade-in 200ms var(--ease-emphasized) forwards",
          }}
          onClick={(e) => e.stopPropagation()}
        >
          {/* 搜索输入框 */}
          <div
            class="flex items-center gap-2.5 px-4 py-3"
            style={{ "border-bottom": "1px solid var(--color-border-subtle)" }}
          >
            <span
              style={{ color: "var(--color-text-tertiary)" }}
              class="shrink-0"
            >
              <Icon name="magnifying-glass" class="w-4 h-4" />
            </span>
            <input
              ref={inputRef}
              type="text"
              class="flex-1 bg-transparent focus:outline-none focus-visible"
              style={{
                "font-size": "14px",
                color: "var(--color-text-primary)",
              }}
              placeholder={t("commandPalette.searchPlaceholder")}
              value={query()}
              onInput={(e) => {
                setQuery(e.currentTarget.value);
                setActiveIndex(0);
              }}
              autofocus
            />
            <kbd
              class="shrink-0 mono rounded px-1.5 py-0.5"
              style={{
                "font-size": "10px",
                color: "var(--color-text-tertiary)",
                background: "var(--color-bg-raised)",
                border: "1px solid var(--color-border-default)",
              }}
            >
              Esc
            </kbd>
          </div>

          {/* 结果列表 */}
          <div
            ref={listRef}
            class="overflow-y-auto py-1.5"
            role="listbox"
            aria-label={t("commandPalette.listAria")}
          >
            <Show when={results().length === 0}>
              <div
                class="px-4 py-6 text-center"
                style={{
                  "font-size": "13px",
                  color: "var(--color-text-tertiary)",
                }}
              >
                {t("commandPalette.noResults")}
              </div>
            </Show>

            <For each={groupOrder}>
              {(gkey) => (
                <Show when={grouped()[gkey].length > 0}>
                  <div
                    class="px-3 py-1.5 uppercase tracking-wider select-none"
                    style={{
                      "font-size": "10px",
                      "font-weight": 600,
                      color: "var(--color-text-tertiary)",
                    }}
                  >
                    {t(GROUP_LABEL_KEYS[gkey])}
                  </div>
                  <For each={grouped()[gkey]}>
                    {(entry) => {
                      const flatIndex = () =>
                        results().findIndex((r) => r.item.id === entry.cmd.id);
                      const isActive = () => activeIndex() === flatIndex();
                      return (
                        <button
                          data-cmd-index={flatIndex()}
                          class="w-full flex items-center gap-3 px-3 py-2 text-left"
                          style={{
                            "font-size": "13px",
                            transition:
                              "background var(--duration-fast) var(--ease-standard)",
                            background: isActive()
                              ? "var(--color-accent-soft)"
                              : "transparent",
                            color: isActive()
                              ? "var(--color-text-primary)"
                              : "var(--color-text-secondary)",
                          }}
                          onClick={() => entry.cmd.run(ctx)}
                          onMouseEnter={() => setActiveIndex(flatIndex())}
                          role="option"
                          aria-selected={isActive()}
                        >
                          <span
                            class="shrink-0"
                            style={{
                              color: isActive()
                                ? "var(--color-accent-primary)"
                                : "var(--color-text-tertiary)",
                            }}
                          >
                            <Icon name={entry.cmd.icon} class="w-4 h-4" />
                          </span>
                          <div class="flex-1 min-w-0">
                            <span class="block truncate">
                              {entry.taskFileName
                                ? (() => {
                                    // 任务条目:"打开任务" 前缀 + 高亮文件名
                                    const prefix = t("command.task.openTask");
                                    return (
                                      <>
                                        <span
                                          style={{
                                            color: "var(--color-text-tertiary)",
                                          }}
                                        >
                                          {prefix}:{" "}
                                        </span>
                                        {highlight(
                                          entry.taskFileName,
                                          entry.indices,
                                        )}
                                      </>
                                    );
                                  })()
                                : highlight(t(entry.cmd.labelKey), entry.indices)}
                            </span>
                            <Show when={entry.cmd.hintKey}>
                              <span
                                class="block truncate"
                                style={{
                                  "font-size": "11px",
                                  color: "var(--color-text-tertiary)",
                                }}
                              >
                                {t(entry.cmd.hintKey!)}
                              </span>
                            </Show>
                          </div>
                          {/* 快捷键提示(若有) */}
                          <Show when={entry.cmd.shortcut}>
                            <span class="flex items-center gap-0.5 shrink-0">
                              <For each={entry.cmd.shortcut}>
                                {(key) => (
                                  <kbd
                                    class="mono rounded px-1 py-0.5"
                                    style={{
                                      "font-size": "10px",
                                      color: "var(--color-text-tertiary)",
                                      background: "var(--color-bg-raised)",
                                      border:
                                        "1px solid var(--color-border-default)",
                                    }}
                                  >
                                    {key}
                                  </kbd>
                                )}
                              </For>
                            </span>
                          </Show>
                        </button>
                      );
                    }}
                  </For>
                </Show>
              )}
            </For>
          </div>

          {/* 底部提示 */}
          <div
            class="flex items-center gap-4 px-4 py-2"
            style={{
              "border-top": "1px solid var(--color-border-subtle)",
              "font-size": "10px",
              color: "var(--color-text-tertiary)",
            }}
          >
            <span class="flex items-center gap-1">
              <kbd
                class="rounded px-1 py-0.5 mono"
                style={{
                  background: "var(--color-bg-raised)",
                  border: "1px solid var(--color-border-default)",
                }}
              >
                ↑
              </kbd>
              <kbd
                class="rounded px-1 py-0.5 mono"
                style={{
                  background: "var(--color-bg-raised)",
                  border: "1px solid var(--color-border-default)",
                }}
              >
                ↓
              </kbd>
              导航
            </span>
            <span class="flex items-center gap-1">
              <kbd
                class="rounded px-1 py-0.5 mono"
                style={{
                  background: "var(--color-bg-raised)",
                  border: "1px solid var(--color-border-default)",
                }}
              >
                Enter
              </kbd>
              执行
            </span>
            <span class="flex items-center gap-1 ml-auto">
              <kbd
                class="rounded px-1 py-0.5 mono"
                style={{
                  background: "var(--color-bg-raised)",
                  border: "1px solid var(--color-border-default)",
                }}
              >
                Ctrl+/
              </kbd>
              全部快捷键
            </span>
          </div>
        </div>
      </div>
    </Show>
  );
}
