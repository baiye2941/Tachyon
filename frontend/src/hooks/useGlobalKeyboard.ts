import { onMount, onCleanup } from "solid-js";
import {
  openNewTaskModal,
  openCommandPalette,
  openShortcutHelp,
  toggleSidebar,
  openView,
  $ui,
} from "../stores/ui";
import { pauseAll, resumeAll, deleteSelected } from "../stores/batchActions";
import {
  $selectedIds,
  hasSelection,
  selectAll,
  deselectAll,
} from "../stores/selection";
import { $taskFilter } from "../stores/taskFilter";
import { SHORTCUTS } from "../commands/shortcuts";
import { matchKeyboardEvent } from "../stores/shortcuts";

/**
 * 全局键盘快捷键:
 * - 所有绑定从 stores/shortcuts.ts 读取,支持用户自定义。
 * - 在输入框/文本区域内不拦截,保留原生编辑行为。
 * - 命令面板/快捷键帮助已打开时不拦截,避免重复触发或冲突。
 * - ?(无修饰键):快捷键帮助别名。
 */
export function useGlobalKeyboard() {
  function isTextInput(target: EventTarget | null): boolean {
    const el = target as HTMLElement | null;
    const tag = el?.tagName;
    return tag === "INPUT" || tag === "TEXTAREA" || Boolean(el?.isContentEditable);
  }

  function handleGlobalKey(e: KeyboardEvent) {
    if (e.repeat) return;
    if (isTextInput(e.target)) return;
    if ($ui.commandPaletteOpen() || $ui.shortcutHelpOpen()) return;

    // Delete: 全局删除当前选中任务(原 BatchToolbar 职责,合并后保留)
    if (e.key === "Delete" && hasSelection()) {
      e.preventDefault();
      deleteSelected();
      return;
    }

    // Ctrl+A / Cmd+A: 全选/取消全选当前过滤列表(与 TaskList 内行为一致)
    if (
      (e.key === "a" || e.key === "A") &&
      (e.ctrlKey || e.metaKey)
    ) {
      e.preventDefault();
      const allIds = $taskFilter.filteredTasks().map((t) => t.id);
      if ($selectedIds.get().size === allIds.length) {
        deselectAll();
      } else {
        selectAll(allIds);
      }
      return;
    }

    for (const s of SHORTCUTS) {
      if (!matchKeyboardEvent(e, s.labelKey)) continue;

      switch (s.labelKey) {
        case "shortcut.openCommandPalette": {
          e.preventDefault();
          openCommandPalette();
          return;
        }
        case "shortcut.shortcutHelp": {
          e.preventDefault();
          openShortcutHelp();
          return;
        }
        case "shortcut.toggleSidebar": {
          e.preventDefault();
          toggleSidebar();
          return;
        }
        case "shortcut.nav.downloads": {
          e.preventDefault();
          openView("downloads");
          return;
        }
        case "shortcut.nav.sniffer": {
          e.preventDefault();
          openView("sniffer");
          return;
        }
        case "shortcut.nav.settings": {
          e.preventDefault();
          openView("settings");
          return;
        }
        case "shortcut.task.new": {
          e.preventDefault();
          openNewTaskModal();
          return;
        }
        case "shortcut.task.pauseAll": {
          e.preventDefault();
          pauseAll();
          return;
        }
        case "shortcut.task.resumeAll": {
          e.preventDefault();
          resumeAll();
          return;
        }
        default:
          break;
      }
    }

    // ?(无修饰键,且非输入框聚焦):快捷键帮助别名
    if (e.key === "?" && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      openShortcutHelp();
    }
  }

  onMount(() => window.addEventListener("keydown", handleGlobalKey));
  onCleanup(() => window.removeEventListener("keydown", handleGlobalKey));
}
