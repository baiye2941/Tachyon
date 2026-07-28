/// <reference types="node" />
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// 动画性能改造契约测试:文本断言(vitest 的 cwd = frontend,用 node fs 读源文件,
// 风格参考 components/__tests__/lintColorHardcode.spec.ts 与 taskRowStyles.spec.ts)
const readSrc = (rel: string): string =>
  readFileSync(join(process.cwd(), rel), "utf-8");

/** 提取指定 @keyframes 的完整块文本(大括号配平,嵌套百分比特写安全) */
function extractKeyframes(css: string, name: string): string {
  const start = css.indexOf(`@keyframes ${name}`);
  if (start < 0) return "";
  const open = css.indexOf("{", start);
  let depth = 0;
  for (let i = open; i < css.length; i++) {
    const ch = css[i];
    if (ch === "{") depth++;
    else if (ch === "}") {
      depth--;
      if (depth === 0) return css.slice(start, i + 1);
    }
  }
  return "";
}

/** 提取指定选择器的首个规则块文本(非嵌套规则) */
function extractRule(css: string, selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const m = css.match(new RegExp(`${escaped}\\s*\\{[^}]*\\}`));
  return m ? m[0] : "";
}

const keyframesCss = readSrc("src/styles/keyframes.css");
const progressCss = readSrc("src/styles/components/progress.css");
const statusBarCss = readSrc("src/styles/components/status-bar.css");
const miscCss = readSrc("src/styles/components/misc.css");

describe("动画性能改造(合成层化 + 消除常驻 paint)", () => {
  describe("契约 a:LiquidProgress fill 合成层化", () => {
    it("fill 的 transition 不含 width(每 tick 触发主线程 layout 的旧实现已移除)", () => {
      const src = readSrc("src/components/LiquidProgress.tsx");
      expect(src).not.toMatch(/transition:[^;]*\bwidth\b/);
    });

    it("fill 宽度固定 100%,进度用 scaleX + transform-origin: left center 表达", () => {
      const src = readSrc("src/components/LiquidProgress.tsx");
      expect(src).toContain("scaleX(");
      expect(src).toContain("transform-origin");
      expect(src).toContain("left center");
    });

    it("transition 只保留 transform 与 background", () => {
      const src = readSrc("src/components/LiquidProgress.tsx");
      expect(src).toContain("transform 320ms");
      expect(src).toContain("background 300ms");
    });

    it("极小进度可见性:scaleX 下限 clamp 替代原 min-width 语义", () => {
      const src = readSrc("src/components/LiquidProgress.tsx");
      // 原实现用 min-width: height px 兜底,合成层化后 transform 不再响应 min-width
      expect(src).not.toContain('"min-width"');
      expect(src).toMatch(/Math\.max\([^)]*SCALE/);
    });

    it("progress.css 轨道加 contain: layout paint(布局/绘制隔离)", () => {
      const block = extractRule(progressCss, ".progress-track-inset");
      expect(block).not.toBe("");
      expect(block).toContain("contain: layout paint");
    });
  });

  describe("契约 b:status-breathe 改伪元素光晕(opacity/transform 可合成)", () => {
    it("status-breathe keyframes 不再动 box-shadow(常驻 paint 源)", () => {
      const block = extractKeyframes(keyframesCss, "status-breathe");
      expect(block).not.toBe("");
      expect(block).not.toContain("box-shadow");
      expect(block).toContain("opacity");
      expect(block).toContain("transform");
    });

    it("status-indicator-active 的呼吸动画移到 ::after 伪元素光晕层", () => {
      const block = extractRule(statusBarCss, ".status-indicator-active::after");
      expect(block).not.toBe("");
      expect(block).toContain("animation: status-breathe");
      // 本体不再直接跑动画(动画在伪元素上)
      const base = extractRule(statusBarCss, ".status-indicator-active");
      expect(base).not.toContain("animation");
    });
  });

  describe("契约 c:shimmer 改 transform 平移渐变层(参考 chunk-shine)", () => {
    it("shimmer keyframes 不再动 background-position", () => {
      const block = extractKeyframes(keyframesCss, "shimmer");
      expect(block).not.toBe("");
      expect(block).not.toContain("background-position");
      expect(block).toContain("transform");
      expect(block).toContain("translateX");
    });

    it(".hf-skeleton 基体不跑背景动画,微光渐变层在 ::before 上平移", () => {
      const base = extractRule(miscCss, ".hf-skeleton");
      expect(base).not.toBe("");
      expect(base).not.toContain("background-position");
      expect(base).not.toContain("animation");
      const before = extractRule(miscCss, ".hf-skeleton::before");
      expect(before).toContain("animation: shimmer");
      expect(before).toContain("linear-gradient");
    });
  });

  describe("契约 d:StatusBadge 状态瞬变平滑化", () => {
    it(".status-badge 基类含 color/background-color/border-color 200ms ease 过渡", () => {
      const block = extractRule(statusBarCss, ".status-badge");
      expect(block).not.toBe("");
      expect(block).toContain("transition");
      expect(block).toMatch(/\bcolor 200ms ease/);
      expect(block).toContain("background-color 200ms ease");
      expect(block).toContain("border-color 200ms ease");
    });
  });

  describe("兜底:reduced-motion 全局降级不被破坏", () => {
    it("base.css 保留 prefers-reduced-motion 全局动画/过渡降级", () => {
      const base = readSrc("src/styles/base.css");
      expect(base).toContain("prefers-reduced-motion: reduce");
      expect(base).toContain("animation-duration: 0.01ms !important");
      expect(base).toContain("transition-duration: 0.01ms !important");
    });
  });
});
