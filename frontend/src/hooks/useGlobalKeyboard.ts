import { onMount, onCleanup } from "solid-js";
import {
  openNewTaskModal,
  openCommandPalette,
  openShortcutHelp,
  toggleSidebar,
  openView,
  $ui,
} from "../stores/ui";
import { pauseAll, resumeAll } from "../stores/batchActions";
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
