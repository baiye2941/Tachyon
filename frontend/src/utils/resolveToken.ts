/**
 * Canvas/脚本场景的 CSS 变量解析器。
 *
 * 语义 token(如 `var(--color-status-downloading)`)只能在 DOM 样式中生效,
 * Canvas 的 `ctx.fillStyle` / `ctx.strokeStyle` 无法解析 CSS 变量,必须传入
 * 解析后的具体颜色值。本模块在首次调用时通过 `getComputedStyle` 读取根元素
 * 的 token 真实值并缓存,后续直接命中缓存,避免重复触发样式查询。
 *
 * 设计原则:
 * - DOM 渲染一律用 `var(--color-*)`,不经过本模块。
 * - Canvas 渲染(ChunkMatrix、Sparkline 等)用 `resolveToken()`。
 * - token 值在运行时确定,因此 index.css 调整品牌色后无需重新构建。
 */

const tokenCache = new Map<string, string>()

// 监听主题切换,自动清除缓存以保证 Canvas 颜色与主题同步
if (typeof document !== 'undefined') {
  const observer = new MutationObserver(() => {
    tokenCache.clear()
  })
  observer.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ['data-theme'],
  })
}

/**
 * 解析一个 CSS 变量名为具体颜色值。
 *
 * @param token 变量名,如 `'--color-status-downloading'`(不含 `var()`)
 * @returns 解析后的颜色值;若变量不存在则回退到 `'#888'`(中性灰,保证可见)
 */
export function resolveToken(token: string): string {
  const cached = tokenCache.get(token)
  if (cached !== undefined) return cached

  // 非浏览器/SSR 环境直接回退
  if (typeof document === 'undefined') {
    const fallback = '#888888'
    tokenCache.set(token, fallback)
    return fallback
  }

  const raw = getComputedStyle(document.documentElement)
    .getPropertyValue(token)
    .trim()
  const value = raw || '#888888'
  tokenCache.set(token, value)
  return value
}

/**
 * 清空 token 缓存。仅用于测试或主题热切换场景。
 */
export function clearTokenCache(): void {
  tokenCache.clear()
}
