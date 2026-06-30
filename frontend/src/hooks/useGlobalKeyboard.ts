import { onMount, onCleanup } from "solid-js";
import {
  openNewTaskModal,
  toggleCommandPalette,
  toggleShortcutHelp,
  toggleSidebar,
} from "../stores/ui";

/**
 * 全局键盘快捷键:
 * - Ctrl/Cmd+K:命令面板
 * - Ctrl/Cmd+/:快捷键帮助
 * - ?(非输入框):快捷键帮助
 * - Ctrl/Cmd+B:切换侧边栏(Iteration 13)
 * - Ctrl/Cmd+N:新建下载
 */
export function useGlobalKeyboard() {
  function isTextInput(target: EventTarget | null): boolean {
    const el = target as HTMLElement | null;
    const tag = el?.tagName;
    return tag === "INPUT" || tag === "TEXTAREA" || Boolean(el?.isContentEditable);
  }

  function handleGlobalKey(e: KeyboardEvent) {
    // Ctrl+K:命令面板
    if ((e.ctrlKey || e.metaKey) && e.key === "k") {
      e.preventDefault();
      toggleCommandPalette();
      return;
    }

    // Ctrl+B:切换侧边栏(Iteration 13)
    if ((e.ctrlKey || e.metaKey) && e.key === "b") {
      e.preventDefault();
      toggleSidebar();
      return;
    }

    // Ctrl+N:新建下载。输入框内不拦截,保留浏览器/编辑器原生行为。
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "n") {
      if (isTextInput(e.target)) return;
      e.preventDefault();
      openNewTaskModal();
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
      if (!isTextInput(target)) {
        e.preventDefault();
        toggleShortcutHelp();
      }
    }
  }

  onMount(() => window.addEventListener("keydown", handleGlobalKey));
  onCleanup(() => window.removeEventListener("keydown", handleGlobalKey));
}
