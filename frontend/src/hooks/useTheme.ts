import { createSignal, onMount } from "solid-js";

/**
 * 主题类型与持久化 key。
 *
 * 与 theme-bootstrap.ts 保持一致:bootstrap 在模块加载时同步读取 localStorage
 * 并设置 data-theme,避免 FOUC;本 hook 提供 SSR 安全的响应式读写入口,
 * 供 StatusBar 切换按钮与命令面板调用。
 */
export type Theme = "dark" | "light";
const THEME_STORAGE_KEY = "tachyon-theme";

function readStoredTheme(): Theme {
  if (typeof localStorage === "undefined") return "dark";
  const raw = localStorage.getItem(THEME_STORAGE_KEY);
  return raw === "light" || raw === "dark" ? raw : "dark";
}

function applyTheme(theme: Theme): void {
  if (typeof document === "undefined") return;
  document.documentElement.setAttribute("data-theme", theme);
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

  const setTheme = (next: Theme) => {
    setThemeSignal(next);
    try {
      localStorage.setItem(THEME_STORAGE_KEY, next);
    } catch {
      /* localStorage 不可用时静默降级 */
    }
    applyTheme(next);
  };

  const toggleTheme = () => setTheme(theme() === "dark" ? "light" : "dark");

  return { theme, setTheme, toggleTheme };
}

/** 顶层导出:供命令面板等非组件场景读取当前主题信号 */
export { theme as currentTheme };
