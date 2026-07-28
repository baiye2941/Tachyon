import { createEffect, onCleanup } from "solid-js";

export interface FocusTrapOptions {
  /** 是否激活焦点陷阱(支持 getter,避免 solid/reactivity 警告) */
  active: boolean | (() => boolean);
  /** 容器元素(含可聚焦子元素);支持 getter 以追踪 ref 解析时机 */
  container: HTMLElement | undefined | (() => HTMLElement | undefined);
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

function resolveContainer(
  container: HTMLElement | undefined | (() => HTMLElement | undefined),
): HTMLElement | undefined {
  return typeof container === "function" ? container() : container;
}

// 模块级焦点陷阱栈:多层弹层叠加(如 DetailPanel 上开 NewTaskModal)时,
// 仅栈顶陷阱响应 Escape 与 Tab 循环,避免一次按键同时关闭两层
const trapStack: symbol[] = [];

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
    const container = resolveContainer(options.container);
    const { autoFocus = true, onEscape, restoreFocus } = options;

    if (!active || !container) return;

    // 注册入栈,cleanup 时出栈
    const trapId = Symbol("focus-trap");
    trapStack.push(trapId);

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
      // 仅栈顶陷阱响应键盘,避免叠加弹层 Esc 双关/Tab 串层
      if (trapStack[trapStack.length - 1] !== trapId) return;

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
      // 出栈(允许非栈顶先清理,保持栈完整)
      const stackIndex = trapStack.indexOf(trapId);
      if (stackIndex >= 0) trapStack.splice(stackIndex, 1);
      if (previouslyFocused && "focus" in previouslyFocused) {
        previouslyFocused.focus();
        previouslyFocused = null;
      }
    });
  });
}

export { FOCUSABLE_SELECTOR };
