import { createSignal, onCleanup } from "solid-js";

function getInitialReducedMotion(): boolean {
  if (typeof window === "undefined" || !window.matchMedia) return false;
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/**
 * 检测用户是否偏好减少动画(Iteration 08)。
 *
 * 用于 JS 驱动的动画决策(如 Canvas 粒子、自定义过渡)。
 * CSS 动画已在 index.css 的 @media (prefers-reduced-motion: reduce) 中全局降级。
 *
 * @returns 返回 signal getter,调用方在 tracked scope 内读取
 */
export function useReducedMotion(): () => boolean {
  const [reduced, setReduced] = createSignal(getInitialReducedMotion());

  // 同步注册监听器,保证调用方立即可用;onCleanup 在组件卸载时移除
  if (typeof window !== "undefined" && window.matchMedia) {
    const mql = window.matchMedia("(prefers-reduced-motion: reduce)");
    const handler = (e: MediaQueryListEvent) => setReduced(e.matches);
    mql.addEventListener("change", handler);
    onCleanup(() => mql.removeEventListener("change", handler));
  }

  return reduced;
}
