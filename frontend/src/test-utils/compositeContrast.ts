/**
 * Canvas/CSS 颜色合成可见性断言工具。
 *
 * 用途:防「渐变/填充画了但看不见」的回归——当 overlay 与底色是同色
 * 低透明度叠加时,alpha 合成结果与底色恒等,字节进度等视觉信息失效
 * (jsdom 无法实测渲染结果,只能用合成数学断言)。
 */

export interface Rgba {
  r: number;
  g: number;
  b: number;
  a: number;
}

/** 解析 `rgb(...)` / `rgba(...)` / `#rrggbb` 为归一化 RGBA(通道 0-1) */
export function parseColor(input: string): Rgba {
  const s = input.trim();
  const hex = /^#([0-9a-f]{6})$/i.exec(s);
  if (hex) {
    const n = parseInt(hex[1]!, 16);
    return {
      r: ((n >> 16) & 0xff) / 255,
      g: ((n >> 8) & 0xff) / 255,
      b: (n & 0xff) / 255,
      a: 1,
    };
  }
  const fn = /^rgba?\(\s*([^)]+)\)$/i.exec(s);
  if (fn) {
    const parts = fn[1]!.split(",").map((p) => parseFloat(p.trim()));
    if (parts.length < 3 || parts.some(Number.isNaN)) {
      throw new Error(`无法解析颜色: ${input}`);
    }
    return {
      r: parts[0]! / 255,
      g: parts[1]! / 255,
      b: parts[2]! / 255,
      a: parts.length >= 4 ? parts[3]! : 1,
    };
  }
  throw new Error(`无法解析颜色: ${input}`);
}

/** 标准 alpha 合成:fg over bg */
export function compositeOver(fg: Rgba, bg: Rgba): Rgba {
  const a = fg.a + bg.a * (1 - fg.a);
  if (a === 0) return { r: 0, g: 0, b: 0, a: 0 };
  return {
    r: (fg.r * fg.a + bg.r * bg.a * (1 - fg.a)) / a,
    g: (fg.g * fg.a + bg.g * bg.a * (1 - fg.a)) / a,
    b: (fg.b * fg.a + bg.b * bg.a * (1 - fg.a)) / a,
    a,
  };
}

/** RGB 欧氏距离(0-√3 空间) */
export function colorDistance(x: Rgba, y: Rgba): number {
  return Math.hypot(x.r - y.r, x.g - y.g, x.b - y.b);
}

/**
 * 断言「overlay 叠加在 base 上」与「base 单独」之间有足够的视觉差异。
 *
 * 两者都先合成到指定背景上再比较(视觉差异主要来自 alpha 不同导致的
 * 背景透出差异,直接比 RGB 会漏判)。默认同时用纯黑与纯白两种背景取
 * 最差值,保证亮/暗主题下都可区分。
 *
 * @param base 底色(下层)
 * @param overlay 叠加色(上层,如渐变端点)
 * @param minDelta 最小 RGB 距离,默认 0.12(约 7% 亮度差,小格子上可感知)
 * @param backdrops 背景色列表,默认纯黑+纯白
 */
export function expectOverlayVisible(
  base: string,
  overlay: string,
  minDelta = 0.12,
  backdrops: [Rgba, ...Rgba[]] = [
    { r: 0, g: 0, b: 0, a: 1 },
    { r: 1, g: 1, b: 1, a: 1 },
  ],
): void {
  const bg = parseColor(base);
  const fg = parseColor(overlay);
  const withOverlay = compositeOver(fg, bg);
  const minDist = Math.min(
    ...backdrops.map((bd) =>
      colorDistance(compositeOver(withOverlay, bd), compositeOver(bg, bd)),
    ),
  );
  if (minDist < minDelta) {
    throw new Error(
      `overlay 叠加后与底色不可区分(距离 ${minDist.toFixed(4)} < ${minDelta})。` +
        `底色 ${base} 与 overlay ${overlay} 属同色低透明度叠加,填充进度在视觉上不存在。`,
    );
  }
}
