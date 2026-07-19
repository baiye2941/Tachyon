import { describe, it, expect } from "vitest";
import {
  parseColor,
  compositeOver,
  colorDistance,
  expectOverlayVisible,
} from "../compositeContrast";

describe("compositeContrast", () => {
  it("parseColor 解析 rgba / rgb / hex", () => {
    expect(parseColor("rgba(53, 221, 226, 0.28)")).toEqual({
      r: 53 / 255,
      g: 221 / 255,
      b: 226 / 255,
      a: 0.28,
    });
    expect(parseColor("rgb(1, 2, 3)").a).toBe(1);
    expect(parseColor("#35dde2")).toEqual({
      r: 53 / 255,
      g: 221 / 255,
      b: 226 / 255,
      a: 1,
    });
    expect(() => parseColor("not-a-color")).toThrow();
  });

  it("compositeOver:同色低透明度叠加与底色恒等(alpha 合成特性)", () => {
    // rgba(C, α) over rgb(C) 的有效颜色仍是 C —— 这正是「同色叠加不可见」的根因
    const base = parseColor("#35dde2");
    const overlay = parseColor("rgba(53, 221, 226, 0.55)");
    const result = compositeOver(overlay, base);
    expect(colorDistance(result, base)).toBeLessThan(1e-9);
  });

  it("expectOverlayVisible:同色叠加拒绝,拉开层次放行", () => {
    // 同色叠加应抛错(填充不可见)
    expect(() =>
      expectOverlayVisible("#35dde2", "rgba(53, 221, 226, 0.55)"),
    ).toThrow(/不可区分/);
    // 低 alpha 底色 + 全强度 overlay 应放行(0.28C → 1.0C 层次)
    expect(() =>
      expectOverlayVisible("rgba(53, 221, 226, 0.28)", "rgba(53, 221, 226, 1)"),
    ).not.toThrow();
  });

  it("expectOverlayVisible:minDelta 阈值可调", () => {
    // 差异极小的双色在宽松阈值下放行,严格阈值下拒绝
    expect(() =>
      expectOverlayVisible("rgba(0, 0, 0, 1)", "rgba(10, 10, 10, 1)", 0.01),
    ).not.toThrow();
    expect(() =>
      expectOverlayVisible("rgba(0, 0, 0, 1)", "rgba(10, 10, 10, 1)", 0.5),
    ).toThrow();
  });
});
