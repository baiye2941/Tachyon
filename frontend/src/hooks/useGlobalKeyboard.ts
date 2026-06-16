import { onMount, onCleanup } from "solid-js";
import {
  toggleCommandPalette,
  toggleShortcutHelp,
} from "../stores/ui";

/**
 * 全局键盘快捷键：Ctrl/Cmd+K 打开命令面板，Ctrl/Cmd+/ 或 ? 打开快捷键帮助。
 */
export function useGlobalKeyboard() {
  function handleGlobalKey(e: KeyboardEvent) {
    // Ctrl+K:命令面板
    if ((e.ctrlKey || e.metaKey) && e.key === "k") {
      e.preventDefault();
      toggleCommandPalette();
      return;
    }

    // Ctrl+/ 或 Cmd+/:快捷键帮助
    if ((e.ctrlKey || e.metaKey) && e.key === "/") {
      e.preventDefault();
      toggleShortcutHelp();
      return;
    }

    // ?(无修饰键,且非输入框聚焦):快捷键帮助
    if (e.key === "?" && !e.ctrlKey && !e.metaKey && !e.altKey) {
      const target = e.target as HTMLElement | null;
      const tag = target?.tagName;
      if (tag !== "INPUT" && tag !== "TEXTAREA" && !target?.isContentEditable) {
        e.preventDefault();
        toggleShortcutHelp();
      }
    }
  }

  onMount(() => window.addEventListener("keydown", handleGlobalKey));
  onCleanup(() => window.removeEventListener("keydown", handleGlobalKey));
}
