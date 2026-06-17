import {
  Show,
  For,
  createMemo,
  createEffect,
  onCleanup,
  createSignal,
} from "solid-js";
import type { TaskInfo } from "../types";
import type { JSX } from "solid-js";
import {
  PauseIcon,
  PlayIcon,
  FolderOpenIcon,
  LinkIcon,
  RefreshIcon,
  TrashIcon,
} from "./icons";
import { tr, type MessageKey } from "../i18n";

interface MenuItem {
  id: string;
  label: string;
  icon?: () => JSX.Element;
  danger?: boolean;
  separator?: boolean;
  action: () => void;
}

interface ContextMenuProps {
  x: number;
  y: number;
  visible: boolean;
  task: TaskInfo | null;
  onClose: () => void;
  onPause: (taskId: string) => void;
  onResume: (taskId: string) => void;
  onOpenFolder: (taskId: string) => void;
  onCopyLink: (taskId: string) => void;
  onRedownload: (taskId: string) => void;
  onDelete: (taskId: string) => void;
}

const MENU_MIN_WIDTH = 180;
const MENU_PADDING_Y = 6;
const MENU_ITEM_HEIGHT = 40; // 32px item + 8px gap-ish

export default function ContextMenu(props: ContextMenuProps) {
  const t = (key: MessageKey) => tr(key);
  let menuRef: HTMLDivElement | undefined;
  let itemRefs: (HTMLButtonElement | undefined)[] = [];
  const [adjustedPos, setAdjustedPos] = createSignal<{ x: number; y: number }>({
    x: 0,
    y: 0,
  });

  const canPause = () =>
    props.task?.status === "downloading" || props.task?.status === "connecting";
  const canResume = () => props.task?.status === "paused";
  const isCompleted = () => props.task?.status === "completed";

  const menuItems = createMemo<MenuItem[]>(() => {
    if (!props.task) return [];
    const items: MenuItem[] = [];

    if (canPause()) {
      items.push({
        id: "pause",
        label: t("common.pause"),
        icon: () => <PauseIcon />,
        action: () => props.onPause(props.task!.id),
      });
    }
    if (canResume()) {
      items.push({
        id: "resume",
        label: t("common.resume"),
        icon: () => <PlayIcon />,
        action: () => props.onResume(props.task!.id),
      });
    }

    items.push({ id: "sep1", label: "", separator: true, action: () => {} });

    if (isCompleted()) {
      items.push({
        id: "open-folder",
        label: t("detail.openFolder"),
        icon: () => <FolderOpenIcon />,
        action: () => props.onOpenFolder(props.task!.id),
      });
    }
    items.push({
      id: "copy-link",
      label: t("detail.copyLink"),
      icon: () => <LinkIcon />,
      action: () => props.onCopyLink(props.task!.id),
    });

    items.push({ id: "sep2", label: "", separator: true, action: () => {} });

    items.push({
      id: "redownload",
      label: t("detail.redownload"),
      icon: () => <RefreshIcon />,
      action: () => props.onRedownload(props.task!.id),
    });
    items.push({
      id: "delete",
      label: t("detail.action.delete"),
      icon: () => <TrashIcon />,
      danger: true,
      action: () => props.onDelete(props.task!.id),
    });

    return items;
  });

  // 视口边界检测:避免菜单超出窗口
  createEffect(() => {
    if (!props.visible) return;
    const items = menuItems();
    const separators = items.filter((i) => i.separator).length;
    const estimatedHeight =
      (items.length - separators) * MENU_ITEM_HEIGHT +
      separators * 9 +
      MENU_PADDING_Y * 2;

    const vw = window.innerWidth;
    const vh = window.innerHeight;
    let x = props.x;
    let y = props.y;

    if (x + MENU_MIN_WIDTH > vw) {
      x = Math.max(0, vw - MENU_MIN_WIDTH - 8);
    }
    if (y + estimatedHeight > vh) {
      y = Math.max(0, vh - estimatedHeight - 8);
    }

    setAdjustedPos({ x, y });
  });

  // 打开时聚焦第一项
  createEffect(() => {
    if (props.visible) {
      requestAnimationFrame(() => {
        const first = itemRefs.find((ref) => ref != null);
        first?.focus();
      });
    }
  });

  const handleKeyDown = (e: KeyboardEvent) => {
    if (!props.visible) return;

    const focusable = itemRefs.filter(Boolean) as HTMLButtonElement[];
    if (focusable.length === 0) return;

    const activeIndex = focusable.findIndex(
      (el) => el === document.activeElement,
    );

    if (e.key === "Escape") {
      e.preventDefault();
      props.onClose();
      return;
    }

    if (e.key === "ArrowDown") {
      e.preventDefault();
      const next = activeIndex >= 0 ? (activeIndex + 1) % focusable.length : 0;
      focusable[next]!.focus();
      return;
    }

    if (e.key === "ArrowUp") {
      e.preventDefault();
      const prev =
        activeIndex >= 0
          ? (activeIndex - 1 + focusable.length) % focusable.length
          : focusable.length - 1;
      focusable[prev]!.focus();
      return;
    }

    if (e.key === "Home") {
      e.preventDefault();
      focusable[0]!.focus();
      return;
    }

    if (e.key === "End") {
      e.preventDefault();
      focusable[focusable.length - 1]!.focus();
      return;
    }

    if ((e.key === "Enter" || e.key === " ") && activeIndex >= 0) {
      e.preventDefault();
      focusable[activeIndex]!.click();
      return;
    }
  };

  createEffect(() => {
    if (props.visible) {
      document.addEventListener("keydown", handleKeyDown);
      onCleanup(() => document.removeEventListener("keydown", handleKeyDown));
    }
  });

  return (
    <Show when={props.visible && props.task}>
      <div
        class="fixed inset-0 z-[150]"
        style={{ background: "transparent" }}
        onClick={() => props.onClose()}
        aria-hidden="true"
      />
      <div
        ref={menuRef}
        role="menu"
        aria-orientation="vertical"
        class="fixed z-[160]"
        style={{
          left: `${adjustedPos().x}px`,
          top: `${adjustedPos().y}px`,
          "min-width": `${MENU_MIN_WIDTH}px`,
          background: "var(--color-bg-elevated)",
          "border-radius": "10px",
          border: "1px solid var(--color-border-default)",
          "box-shadow": "var(--shadow-lg)",
          padding: "6px 0",
          animation: "fadeIn 100ms ease forwards",
        }}
      >
        <For each={menuItems()}>
          {(item, index) => (
            <Show
              when={!item.separator}
              fallback={
                <div
                  role="separator"
                  style={{
                    height: "1px",
                    background: "var(--color-border-subtle)",
                    margin: "4px 8px",
                  }}
                />
              }
            >
              <button
                ref={(el) => {
                  itemRefs[index()] = el;
                }}
                role="menuitem"
                tabindex={-1}
                class={`flex items-center gap-2 w-full text-left ${item.danger ? "hover-danger" : "hover-light"}`}
                style={{
                  height: "32px",
                  padding: "0 12px",
                  "font-size": "14px",
                  color: item.danger
                    ? "var(--color-status-error)"
                    : "var(--color-text-title)",
                  background: "transparent",
                  border: "none",
                  cursor: "pointer",
                  transition: "all 150ms ease",
                  "border-radius": "4px",
                  margin: "0 4px",
                  width: "calc(100% - 8px)",
                }}
                onClick={() => {
                  item.action();
                  props.onClose();
                }}
              >
                <span
                  style={{
                    width: "16px",
                    height: "16px",
                    display: "flex",
                    "align-items": "center",
                  }}
                >
                  {item.icon?.()}
                </span>
                <span>{item.label}</span>
              </button>
            </Show>
          )}
        </For>
      </div>
    </Show>
  );
}
