import { describe, it, expect, afterEach } from "vitest";
import { render, cleanup } from "@solidjs/testing-library";
import AnimatedNumber from "../AnimatedNumber";

function getDigitStrips(container: HTMLElement) {
  return Array.from(container.querySelectorAll(".animated-digit-strip"));
}

function getStripOffset(strip: Element | undefined): number {
  if (!strip) return 0;
  const style = (strip as HTMLElement).style.transform;
  const match = style.match(/translateY\(([-\d.]+)%\)/);
  return match?.[1] ? parseFloat(match[1]) : 0;
}

describe("AnimatedNumber", () => {
  afterEach(() => {
    cleanup();
  });

  it("将每个数字渲染为独立滚轮", () => {
    const { container } = render(() => <AnimatedNumber value="45" />);
    const strips = getDigitStrips(container);
    expect(strips.length).toBe(2);
    expect(getStripOffset(strips[0])).toBe(-40);
    expect(getStripOffset(strips[1])).toBe(-50);
  });

  it("非数字字符作为静态文本保留", () => {
    const { container } = render(() => <AnimatedNumber value="12.3%" />);
    const chars = Array.from(
      container.querySelectorAll(".animated-number-char"),
    ).map((el) => el.textContent ?? "");
    expect(chars).toEqual([".", "%"]);
  });

  it("数值字符串中数字滚到正确位置", () => {
    const { container } = render(() => <AnimatedNumber value="1.2" />);
    const strips = getDigitStrips(container);
    expect(strips.length).toBe(2);
    expect(getStripOffset(strips[0])).toBe(-10);
    expect(getStripOffset(strips[1])).toBe(-20);
  });

  it("支持 number 类型输入", () => {
    const { container } = render(() => <AnimatedNumber value={98.6} />);
    const strips = getDigitStrips(container);
    expect(strips.length).toBe(3);
    expect(getStripOffset(strips[0])).toBe(-90);
    expect(getStripOffset(strips[1])).toBe(-80);
    expect(getStripOffset(strips[2])).toBe(-60);
  });

  it("纯非数字值不创建滚轮", () => {
    const { container } = render(() => <AnimatedNumber value="---" />);
    expect(getDigitStrips(container).length).toBe(0);
    expect(container.textContent).toBe("---");
  });

  it("应用自定义 class", () => {
    const { container } = render(() => (
      <AnimatedNumber value="1" class="my-number" />
    ));
    expect(container.querySelector(".animated-number.my-number")).toBeTruthy();
  });
});
