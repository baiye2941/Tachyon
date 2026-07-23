import {
  createSignal,
  Show,
  For,
  createMemo,
  onCleanup,
  type JSX,
} from "solid-js";
import EmptyState from "../shared/ui/EmptyState";
import { Motion, Presence } from "@motionone/solid";
import { Icon } from "../utils/icons";
import type { ViewName, TaskInfo } from "../types";
import { useReducedMotion } from "../hooks/useReducedMotion";
import { useIsSmallScreen } from "../hooks/useMediaQuery";
import { useFocusTrap } from "../hooks/useFocusTrap";
import {
  COMMANDS,
  GROUP_LABEL_KEYS,
  type Command,
  type CommandContext,
  type CommandGroup,
} from "../commands/registry";
import { fuzzySearch } from "../utils/fuzzySearch";
import { tr, type MessageKey } from "../i18n";
import {
  $recent,
  $pinned,
  addRecentCommand,
  togglePinnedCommand,
  isPinned,
} from "../stores/commandHistory";
import { getCommandShortcutKeys, getShortcutKeys } from "../stores/shortcuts";
import { platformKeys } from "../commands/shortcuts";
import { isMacPlatform } from "../stores/shortcuts";

const GROUP_LABEL_KEYS_EXTENDED = {
  ...GROUP_LABEL_KEYS,
  pinned: "commandGroup.pinned",
  recent: "commandGroup.recent",
} as const satisfies Record<string, MessageKey>;

/** 命令项右侧 shortcut badge:从 shortcuts store 读取当前绑定,并做平台适配 */
function ShortcutBadge(props: { commandId: string; isMac: boolean }) {
  const keys = createMemo(() => getCommandShortcutKeys(props.commandId));
  const displayKeys = createMemo(() =>
    keys() ? platformKeys(keys()!, props.isMac) : undefined,
  );
  return (
    <Show when={displayKeys()} keyed>
      {(k) =>
        k.length > 0 ? (
          <span class="cmd-item-shortcut">
            <For each={k}>
              {(key) => <kbd>{key}</kbd>}
            </For>
          </span>
        ) : null
      }
    </Show>
  );
}

export interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
  onViewChange: (view: ViewName) => void;
  onNewDownload?: () => void;
  onPauseAll?: () => void;
  onResumeAll?: () => void;
  onCancelAll?: () => void;
  onClearCompleted?: () => void;
  onToggleSidebar?: () => void;
  /** 任务搜索数据源(spec 8.6) */
  getTasks?: () => { id: string; fileName: string; url: string }[];
  /** 选中任务后打开详情 */
  onOpenTask?: (taskId: string) => void;
  /** 当前选中的任务(任务级操作命令上下文) */
  getSelectedTask?: () => TaskInfo | null;
  /** 打开选中任务的保存目录 */
  onOpenTaskFolder?: (taskId: string) => void;
  /** 重新下载选中任务 */
  onRedownloadTask?: (taskId: string) => void;
  /** 复制文本到剪贴板 */
  onCopyToClipboard?: (text: string) => void;
  /** 搜索防抖延迟(ms),默认 100ms */
  debounceMs?: number;
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
  return (
    <For each={out}>
      {(p) => (typeof p === "string" ? p : <mark>{p.mark}</mark>)}
    </For>
  ) as JSX.Element;
}

type DisplayItem = {
  key: string;
  cmd: Command;
  indices: number[];
  taskFileName?: string;
};

type SectionKey = "pinned" | "recent" | CommandGroup;

type Section = {
  key: SectionKey;
  label: MessageKey;
  items: DisplayItem[];
};

export default function CommandPalette(props: CommandPaletteProps) {
  let inputRef: HTMLInputElement | undefined;
  let listRef: HTMLDivElement | undefined;
  let trapContainerRef: HTMLDivElement | undefined;
  const t = (key: MessageKey) => tr(key);
  const reducedMotion = useReducedMotion();
  const isSmall = useIsSmallScreen();
  useFocusTrap({
    active: () => props.open,
    container: () => trapContainerRef,
    // Escape 仍由当前组件的 onKeyDown 处理；focus trap 只负责 Tab 循环与焦点恢复。
  });
  const isMac = isMacPlatform();
  const [inputQuery, setInputQuery] = createSignal("");
  const [query, setQuery] = createSignal("");
  const [activeIndex, setActiveIndex] = createSignal(0);

  let debounceTimer: ReturnType<typeof setTimeout> | undefined;
  const setDebouncedQuery = (value: string) => {
    setInputQuery(value);
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => {
      setQuery(value);
      setActiveIndex(0);
    }, props.debounceMs ?? 100);
  };

  onCleanup(() => clearTimeout(debounceTimer));

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
    get onCancelAll() {
      return props.onCancelAll;
    },
    get onClearCompleted() {
      return props.onClearCompleted;
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
    get getSelectedTask() {
      return props.getSelectedTask;
    },
    get onOpenTaskFolder() {
      return props.onOpenTaskFolder;
    },
    get onRedownloadTask() {
      return props.onRedownloadTask;
    },
    get onCopyToClipboard() {
      return props.onCopyToClipboard;
    },
  };

  const visibleCommands = createMemo(() =>
    COMMANDS.filter((c) => (c.visible ? c.visible(ctx) : true)),
  );

  const commandResults = createMemo(() =>
    fuzzySearch(
      visibleCommands(),
      query(),
      (c) =>
        `${t(c.labelKey)} ${c.hintKey ? t(c.hintKey) : ""} ${(c.aliases ?? []).join(" ")}`,
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
    return fuzzySearch(
      tasks,
      query(),
      (task) => `${task.fileName} ${task.url}`,
    ).map((r) => {
      const task = r.item;
      const synthetic: Command = {
        id: `task-open:${task.id}`,
        labelKey: "command.task.openTask",
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
    });
  });

  const groupOrder: CommandGroup[] = ["navigation", "task", "action"];

  const sections = createMemo((): Section[] => {
    if (query().trim().length === 0) {
      const pinnedIds = new Set($pinned);
      const recentIds = new Set($recent);

      const pinnedItems: DisplayItem[] = $pinned
        .map((id) => visibleCommands().find((c) => c.id === id))
        .filter((c): c is Command => !!c)
        .map((cmd) => ({ key: `pinned:${cmd.id}`, cmd, indices: [] }));

      const recentItems: DisplayItem[] = $recent
        .filter((id) => !pinnedIds.has(id))
        .map((id) => visibleCommands().find((c) => c.id === id))
        .filter((c): c is Command => !!c)
        .map((cmd) => ({ key: `recent:${cmd.id}`, cmd, indices: [] }));

      const out: Section[] = [];
      if (pinnedItems.length > 0) {
        out.push({
          key: "pinned",
          label: GROUP_LABEL_KEYS_EXTENDED.pinned,
          items: pinnedItems,
        });
      }
      if (recentItems.length > 0) {
        out.push({
          key: "recent",
          label: GROUP_LABEL_KEYS_EXTENDED.recent,
          items: recentItems,
        });
      }

      for (const g of groupOrder) {
        const items = visibleCommands()
          .filter((c) => c.group === g && !pinnedIds.has(c.id) && !recentIds.has(c.id))
          .map((cmd) => ({ key: `${g}:${cmd.id}`, cmd, indices: [] }));
        if (items.length > 0) {
          out.push({ key: g, label: GROUP_LABEL_KEYS[g], items });
        }
      }
      return out;
    }

    const merged = [...commandResults(), ...taskResults()];
    merged.sort((a, b) => b.score - a.score);

    const byGroup: Record<CommandGroup, DisplayItem[]> = {
      navigation: [],
      action: [],
      task: [],
    };
    for (const r of merged) {
      byGroup[r.item.group].push({
        key: `${r.item.group}:${r.item.id}:${r.taskFileName ?? ""}`,
        cmd: r.item,
        indices: r.matchedIndices,
        taskFileName: r.taskFileName,
      });
    }
    return groupOrder
      .map((g) => ({
        key: g,
        label: GROUP_LABEL_KEYS[g],
        items: byGroup[g],
      }))
      .filter((s) => s.items.length > 0);
  });

  const flattened = createMemo(() => sections().flatMap((s) => s.items));

  const indexByKey = createMemo(() => {
    const map = new Map<string, number>();
    flattened().forEach((it, i) => map.set(it.key, i));
    return map;
  });

  const easterEgg = createMemo(() => {
    const q = query().trim().toLowerCase();
    if (q === "uma") return t("commandPalette.easterEgg.uma");
    if (q === "tachyon") return t("commandPalette.easterEgg.tachyon");
    return null;
  });

  function executeItem(item: DisplayItem) {
    if (!item.taskFileName) {
      addRecentCommand(item.cmd.id);
    }
    item.cmd.run(ctx);
  }

  function executeActive() {
    const item = flattened()[activeIndex()];
    if (!item) return;
    executeItem(item);
  }

  function scrollActiveIntoView() {
    const el = listRef?.querySelector(`[data-cmd-index="${activeIndex()}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (!props.open) return;
    const total = flattened().length;
    switch (e.key) {
      case "Escape":
        e.preventDefault();
        props.onClose();
        break;
      case "ArrowDown":
        e.preventDefault();
        setActiveIndex((i) => (total === 0 ? 0 : (i + 1) % total));
        scrollActiveIntoView();
        break;
      case "ArrowUp":
        e.preventDefault();
        setActiveIndex((i) => (total === 0 ? 0 : (i - 1 + total) % total));
        scrollActiveIntoView();
        break;
      case "Enter":
        e.preventDefault();
        if (e.shiftKey) {
          const item = flattened()[activeIndex()];
          if (item && !item.taskFileName) {
            togglePinnedCommand(item.cmd.id);
          }
        } else {
          executeActive();
        }
        break;
    }
  }

  function handleOverlayClick(e: MouseEvent) {
    if (e.target === e.currentTarget) {
      props.onClose();
    }
  }

  return (
    <Presence>
      <Show when={props.open}>
        <div
          class="fixed inset-0 z-[var(--z-command-palette)] flex items-start justify-center pt-[15vh] px-4"
          role="dialog"
          aria-modal="true"
          aria-label={t("commandPalette.aria")}
          style={{
            background: "var(--color-overlay-scrim)",
          }}
          onClick={handleOverlayClick}
          onKeyDown={handleKeyDown}
          ref={() => {
            requestAnimationFrame(() => inputRef?.focus());
          }}
        >
          <Motion.div
            class="cmd-panel flex flex-col"
            classList={{ "cmd-panel--narrow": isSmall() }}
            initial={{ opacity: 0, y: -12, scale: 0.98 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: -8, scale: 0.98 }}
            transition={
              reducedMotion()
                ? { duration: 0 }
                : {
                    type: "spring",
                    stiffness: 400,
                    damping: 25,
                  }
            }
            onClick={(e: MouseEvent) => e.stopPropagation()}
          >
            <div ref={trapContainerRef} class="contents">
            {/* 搜索输入区 */}
            <div class="cmd-input-wrap">
              <span class="cmd-item-icon">
                <Icon name="magnifying-glass" class="w-5 h-5" />
              </span>
              <input
                ref={inputRef}
                type="text"
                role="combobox"
                aria-expanded="true"
                aria-controls="cmd-palette-listbox"
                aria-autocomplete="list"
                aria-activedescendant={
                  flattened().length > 0 ? `cmd-opt-${activeIndex()}` : undefined
                }
                class="cmd-input"
                placeholder={t("commandPalette.searchPlaceholder")}
                value={inputQuery()}
                onInput={(e) => setDebouncedQuery(e.currentTarget.value)}
                autofocus
              />
              <span class="cmd-esc-hint">Esc</span>
            </div>

            {/* Easter Egg 提示 */}
            <Show when={easterEgg()}>
              <div class="cmd-easter-egg">
                <span class="cmd-easter-egg-icon">✦</span>
                <span>{easterEgg()}</span>
              </div>
            </Show>

            {/* 结果列表 */}
            <div
              ref={listRef}
              id="cmd-palette-listbox"
              class="cmd-list flex-1 scroll-container"
              role="listbox"
              aria-label={t("commandPalette.listAria")}
            >
              <Show when={flattened().length === 0}>
                <EmptyState
                  compact
                  icon={<Icon name="magnifying-glass" class="w-6 h-6" />}
                  title={t("commandPalette.noResults")}
                  description={t("commandPalette.emptyHint")}
                />
              </Show>

              <For each={sections()}>
                {(section) => (
                  <>
                    <div class="cmd-group-label">{t(section.label)}</div>
                    <For each={section.items}>
                      {(entry) => {
                        const globalIndex = () =>
                          indexByKey().get(entry.key)!;
                        const active = () => activeIndex() === globalIndex();
                        return (
                          <div
                            id={`cmd-opt-${globalIndex()}`}
                            data-cmd-index={globalIndex()}
                            class="cmd-item"
                            onClick={() => executeItem(entry)}
                            onMouseEnter={() => setActiveIndex(globalIndex())}
                            role="option"
                            aria-selected={active()}
                          >
                            <span class="cmd-item-icon">
                              <Icon name={entry.cmd.icon} class="w-5 h-5" />
                            </span>
                            <div class="cmd-item-text">
                              <span class="cmd-item-title cmd-palette-mark">
                                {entry.taskFileName
                                  ? highlight(entry.taskFileName, entry.indices)
                                  : highlight(t(entry.cmd.labelKey), entry.indices)}
                              </span>
                              <Show when={entry.cmd.hintKey || entry.taskFileName}>
                                <span class="cmd-item-hint">
                                  {entry.taskFileName
                                    ? t("command.task.openTask")
                                    : t(entry.cmd.hintKey!)}
                                </span>
                              </Show>
                            </div>
                            {/* pin 按钮(仅真实命令,任务合成项不显示) */}
                            <Show when={!entry.taskFileName}>
                              <button
                                type="button"
                                class="cmd-item-pin"
                                aria-label={
                                  isPinned(entry.cmd.id)
                                    ? t("commandPalette.unpinAria")
                                    : t("commandPalette.pinAria")
                                }
                                onClick={(e: MouseEvent) => {
                                  e.stopPropagation();
                                  togglePinnedCommand(entry.cmd.id);
                                }}
                              >
                                <Icon
                                  name={isPinned(entry.cmd.id) ? "pin" : "pin-off"}
                                  class="w-4 h-4"
                                />
                              </button>
                            </Show>
                            {/* 快捷键提示(从配置读取,list 类无 commandId 不显示) */}
                            <ShortcutBadge
                              commandId={entry.cmd.id}
                              isMac={isMac}
                            />
                          </div>
                        );
                      }}
                    </For>
                  </>
                )}
              </For>
            </div>

            {/* a11y:屏幕阅读器播报结果计数,视觉隐藏 */}
            <div aria-live="polite" role="status" class="sr-only">
              {flattened().length > 0
                ? `${flattened().length} ${t("commandPalette.listAria")}`
                : t("commandPalette.noResults")}
            </div>

            {/* 底部提示栏 */}
            <div class="cmd-footer">
              <span class="flex items-center gap-1">
                <kbd>↑</kbd>
                <kbd>↓</kbd>
                {t("commandPalette.hintNav")}
              </span>
              <span class="flex items-center gap-1">
                <kbd>Enter</kbd>
                {t("commandPalette.hintExecute")}
              </span>
              <span class="flex items-center gap-1 ml-auto">
                <For each={platformKeys(getShortcutKeys("shortcut.shortcutHelp"), isMac)}>
                  {(key) => <kbd>{key}</kbd>}
                </For>
                {t("commandPalette.hintAllShortcuts")}
              </span>
            </div>
            </div>
          </Motion.div>
        </div>
      </Show>
    </Presence>
  );
}
