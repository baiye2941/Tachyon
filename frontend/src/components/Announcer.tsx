import { createEffect, createSignal } from "solid-js";
import { toasts } from "./ToastContainer";

/**
 * 全局屏幕阅读器播报区(Phase 4-5 A11y 审计)。
 *
 * 独立于 ToastContainer 的视觉弹窗，提供稳定的 aria-live region：
 * - 只播报最新一条通知的文本，避免多条 toast 同时出现导致读屏混乱。
 * - 使用 polite 模式，不中断用户当前操作。
 * - 挂载在 App.tsx 根节点，确保整个应用生命周期内 live region 始终存在。
 */
export default function Announcer() {
  const [message, setMessage] = createSignal("");

  createEffect(() => {
    const list = toasts();
    const latest = list[list.length - 1];
    if (!latest) return;

    const text = latest.description
      ? `${latest.title} ${latest.description}`
      : latest.title;
    setMessage(text);
  });

  return (
    <div
      class="sr-only"
      role="status"
      aria-live="polite"
      aria-atomic="true"
    >
      {message()}
    </div>
  );
}
