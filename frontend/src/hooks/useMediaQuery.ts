import { createSignal, onCleanup, type Accessor } from "solid-js";

/**
 * 响应式断点定义(Iteration 13)
 *
 * 与 index.css 的 --bp-* token 对齐,供 JS 侧逻辑判断使用
 * (CSS 无法完全覆盖的场景:如 DetailPanel 在窄屏切换为全屏覆盖模式)。
 */
export const BREAKPOINTS = {
  sm: 640,
  md: 768,
  lg: 1024,
  xl: 1280,
} as const;

export type Breakpoint = keyof typeof BREAKPOINTS;

function getInitialMatch(query: string): boolean {
  if (typeof window === "undefined" || !window.matchMedia) return false;
  return window.matchMedia(query).matches;
}

/**
 * 订阅一个媒体查询,返回响应式 signal。
 *
 * - 在组件(reactive)作用域内调用,自动随窗口尺寸变化更新。
 * - onCleanup 自动移除监听器。
 * - SSR/无 matchMedia 环境安全降级返回 false。
 *
 * @param query 媒体查询字符串,如 `(max-width: 768px)`
 */
export function useMediaQuery(query: string): Accessor<boolean> {
  const [matches, setMatches] = createSignal(getInitialMatch(query));

  if (typeof window !== "undefined" && window.matchMedia) {
    const mql = window.matchMedia(query);
    const handler = (e: MediaQueryListEvent) => setMatches(e.matches);
    mql.addEventListener("change", handler);
    onCleanup(() => mql.removeEventListener("change", handler));
  }

  return matches;
}

/**
 * 便捷:是否为窄屏(<= md 断点)。
 * 窄屏下侧边栏强制轨道、DetailPanel 改全屏覆盖。
 */
export function useIsNarrowScreen(): Accessor<boolean> {
  return useMediaQuery(`(max-width: ${BREAKPOINTS.md}px)`);
}

/**
 * 便捷:是否为超窄屏(<= sm 断点)。
 * 超窄屏下批量工具栏收起次要操作到「更多」菜单,命令面板/弹窗接近全屏。
 */
export function useIsSmallScreen(): Accessor<boolean> {
  return useMediaQuery(`(max-width: ${BREAKPOINTS.sm}px)`);
}

/**
 * 便捷:是否为宽屏(>= xl 断点)。
 * 宽屏下 DetailPanel 改为右侧固定侧栏,与列表并列。
 */
export function useIsWideScreen(): Accessor<boolean> {
  return useMediaQuery(`(min-width: ${BREAKPOINTS.xl}px)`);
}
