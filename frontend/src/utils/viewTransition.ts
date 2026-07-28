/**
 * View Transitions API 类型安全封装(主题切换水波扩散用)。
 *
 * WebView2(Chromium 111+)支持;WKWebView、旧浏览器与 jsdom 不支持,
 * 返回 null 由调用方走即时切换降级。
 */

/** 主题过渡扩散起点(视口 CSS 像素坐标) */
export interface TransitionOrigin {
  x: number;
  y: number;
}

interface ViewTransitionLike {
  ready: Promise<void>;
  finished: Promise<void>;
  skipTransition: () => void;
}

type DocumentWithViewTransition = Document & {
  startViewTransition?: (callback: () => void) => ViewTransitionLike;
};

/** 检测当前环境是否支持 View Transitions */
export function supportsViewTransition(): boolean {
  return (
    typeof document !== "undefined" &&
    typeof (document as DocumentWithViewTransition).startViewTransition ===
      "function"
  );
}

/**
 * 启动一次 View Transition。不支持时返回 null,调用方自行同步执行 update。
 * ready 后由调用方对 ::view-transition-new(root) 做 clip-path 圆形扩散动画。
 */
export function startThemeTransition(
  update: () => void,
): ViewTransitionLike | null {
  if (!supportsViewTransition()) return null;
  return (document as DocumentWithViewTransition).startViewTransition!(update);
}
