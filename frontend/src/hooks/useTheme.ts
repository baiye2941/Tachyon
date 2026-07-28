import { createSignal, onMount } from "solid-js";
import {
  startThemeTransition,
  type TransitionOrigin,
} from "../utils/viewTransition";

/**
 * 主题类型与持久化 key。
 *
 * 与 theme-bootstrap.ts 保持一致:bootstrap 在模块加载时同步读取 localStorage
 * 并设置 data-theme,避免 FOUC;本 hook 提供 SSR 安全的响应式读写入口,
 * 供 StatusBar 切换按钮与命令面板调用。
 */
export type Theme = "dark" | "light";
const THEME_STORAGE_KEY = "tachyon-theme";

/** 无点击坐标时的默认扩散起点(左上角,如命令面板触发) */
const DEFAULT_ORIGIN: TransitionOrigin = { x: 16, y: 16 };

function readStoredTheme(): Theme {
  if (typeof localStorage === "undefined") return "dark";
  const raw = localStorage.getItem(THEME_STORAGE_KEY);
  return raw === "light" || raw === "dark" ? raw : "dark";
}

function applyTheme(theme: Theme): void {
  if (typeof document === "undefined") return;
  document.documentElement.setAttribute("data-theme", theme);
}

function prefersReducedMotion(): boolean {
  if (typeof window === "undefined" || !window.matchMedia) return false;
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/**
 * 应用主题,支持时以点击处为圆心做圆形水波扩散(View Transitions)。
 * 降级条件(任一):无 startViewTransition、prefers-reduced-motion —— 即时切换。
 */
function applyThemeWithRipple(theme: Theme, origin: TransitionOrigin): void {
  if (prefersReducedMotion()) {
    applyTheme(theme);
    return;
  }
  const transition = startThemeTransition(() => applyTheme(theme));
  if (!transition) {
    applyTheme(theme);
    return;
  }
  transition.ready
    .then(() => {
      const radius = Math.hypot(
        Math.max(origin.x, window.innerWidth - origin.x),
        Math.max(origin.y, window.innerHeight - origin.y),
      );
      document.documentElement.animate(
        {
          clipPath: [
            `circle(0px at ${origin.x}px ${origin.y}px)`,
            `circle(${radius}px at ${origin.x}px ${origin.y}px)`,
          ],
        },
        {
          duration: 550,
          easing: "cubic-bezier(0.22, 1, 0.36, 1)",
          pseudoElement: "::view-transition-new(root)",
        },
      );
    })
    .catch(() => {
      /* transition 被跳过(如快速连续切换):主题已应用,动画放弃即可 */
    });
}

const [theme, setThemeSignal] = createSignal<Theme>("dark");

/**
 * 主题响应式 hook。
 *
 * onMount 时从 localStorage 同步一次初始值(bootstrap 已设 data-theme,
 * 此处仅同步 signal,确保信号与 DOM 一致)。setTheme 同时写 localStorage
 * 与 data-theme,并触发 resolveToken 缓存清理(MutationObserver 监听 data-theme)。
 */
export function useTheme() {
  onMount(() => {
    setThemeSignal(readStoredTheme());
  });

  const setTheme = (next: Theme, origin?: TransitionOrigin) => {
    setThemeSignal(next);
    try {
      localStorage.setItem(THEME_STORAGE_KEY, next);
    } catch {
      /* localStorage 不可用时静默降级 */
    }
    applyThemeWithRipple(next, origin ?? DEFAULT_ORIGIN);
  };

  const toggleTheme = (origin?: TransitionOrigin) =>
    setTheme(theme() === "dark" ? "light" : "dark", origin);

  return { theme, setTheme, toggleTheme };
}

/** 顶层导出:供命令面板等非组件场景读取当前主题信号 */
export { theme as currentTheme };
