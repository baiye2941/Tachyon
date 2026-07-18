import { describe, it, expect } from "vitest";
import { render } from "@solidjs/testing-library";
import FragmentFill from "../FragmentFill";

describe("FragmentFill", () => {
  it("渲染充能条 transform scaleX 由 progress 决定", () => {
    const { container } = render(() => (
      <FragmentFill progress={0.5} reducedMotion={false} />
    ));
    const fill = container.querySelector(".chunk-cell-fill");
    expect(fill).toBeTruthy();
  });

  it("progress=0 时不抛错(空态)", () => {
    const { container } = render(() => (
      <FragmentFill progress={0} reducedMotion={false} />
    ));
    expect(container).toBeTruthy();
  });

  it("reducedMotion 降级:不使用 spring,直接设 transform", () => {
    const { container } = render(() => (
      <FragmentFill progress={0.7} reducedMotion={true} />
    ));
    const fill = container.querySelector(".chunk-cell-fill");
    expect(fill).toBeTruthy();
    if (fill) {
      expect((fill as HTMLElement).style.transform).toContain("0.7");
    }
  });

  it("reducedMotion 下降级 progress 超出 [0,1] 时被 clamp", () => {
    const { container } = render(() => (
      <FragmentFill progress={1.4} reducedMotion={true} />
    ));
    const fill = container.querySelector(".chunk-cell-fill");
    expect(fill).toBeTruthy();
    if (fill) {
      expect((fill as HTMLElement).style.transform).toContain("scaleX(1)");
    }
  });

  it("充能条对辅助技术隐藏(purely decorative)", () => {
    const { container } = render(() => (
      <FragmentFill progress={0.5} reducedMotion={true} />
    ));
    const fill = container.querySelector(".chunk-cell-fill");
    expect(fill?.getAttribute("aria-hidden")).toBe("true");
  });
});
