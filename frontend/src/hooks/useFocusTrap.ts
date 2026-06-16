import { createEffect, onCleanup } from "solid-js";

export interface FocusTrapOptions {
  /** 是否激活焦点陷阱(支持 getter,避免 solid/reactivity 警告) */
  active: boolean | (() => boolean);
  /** 容器元素(含可聚焦子元素) */
  container: HTMLElement | undefined;
  /** 打开时是否自动聚焦第一个可聚焦元素,默认 true */
  autoFocus?: boolean;
  /** 按 Escape 时回调 */
  onEscape?: () => void;
  /** 关闭后恢复焦点的元素;不传则恢复打开前的 activeElement */
  restoreFocus?: HTMLElement | null;
}

const FOCUSABLE_SELECTOR =
  'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])';

function getFocusable(container: HTMLElement): HTMLElement[] {
  return Array.from(
    container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
  ).filter(
    (el) => !el.hasAttribute("disabled") && !el.getAttribute("aria-hidden"),
  );
}

function resolveActive(active: boolean | (() => boolean)): boolean {
  return typeof active === "function" ? active() : active;
}

/**
 * 焦点陷阱 hook(Iteration 08)。
 *
 * 统一处理 Modal/Dialog/Panel 的键盘可访问性:
 * - 打开时保存当前焦点并(可选)移入第一个可聚焦元素
 * - Tab/Shift+Tab 在容器内循环
 * - Escape 触发 onEscape
 * - 关闭时恢复之前焦点
 *
 * 与具体组件解耦,ConfirmDialog/NewTaskModal/ShortcutHelp/ContextMenu 均可复用。
 */
export function useFocusTrap(options: FocusTrapOptions) {
  let previouslyFocused: HTMLElement | null = null;
  let keyHandler: ((e: KeyboardEvent) => void) | null = null;

  createEffect(() => {
    const active = resolveActive(options.active);
    const { container, autoFocus = true, onEscape, restoreFocus } = options;

    if (!active || !container) return;

    // 保存焦点
    previouslyFocused = (restoreFocus ??
      document.activeElement) as HTMLElement | null;

    // 自动聚焦
    if (autoFocus) {
      requestAnimationFrame(() => {
        const focusable = getFocusable(container);
        const target =
          focusable.find((el) => el.hasAttribute("data-autofocus")) ||
          focusable[0];
        target?.focus();
      });
    }

    keyHandler = (e: KeyboardEvent) => {
      if (e.key === "Escape" && onEscape) {
        e.preventDefault();
        onEscape();
        return;
      }

      if (e.key !== "Tab" || !container) return;
      const focusable = getFocusable(container);
      if (focusable.length === 0) return;

      const first = focusable[0]!;
      const last = focusable[focusable.length - 1]!;

      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    };

    document.addEventListener("keydown", keyHandler);

    onCleanup(() => {
      if (keyHandler) {
        document.removeEventListener("keydown", keyHandler);
        keyHandler = null;
      }
      if (previouslyFocused && "focus" in previouslyFocused) {
        previouslyFocused.focus();
        previouslyFocused = null;
      }
    });
  });
}

export { FOCUSABLE_SELECTOR };
