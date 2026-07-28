import { describe, it, expect, afterEach } from "vitest";
import { render, screen, cleanup } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import Button from "../Button";

// Button loading 态回归:此前 loading 仅 disabled + aria-busy,无视觉表达,
// 用户得不到反馈易重复点击;修复后须渲染 btn-spinner 圆弧指示器。
describe("Button loading 状态", () => {
  afterEach(() => {
    cleanup();
  });

  it("loading 时渲染 btn-spinner(aria-hidden)且按钮 disabled + aria-busy", () => {
    const { container } = render(() => <Button loading>提交中</Button>);
    const button = screen.getByRole("button");
    const spinner = container.querySelector(".btn-spinner");
    expect(spinner).not.toBeNull();
    expect(spinner?.getAttribute("aria-hidden")).toBe("true");
    expect((button as HTMLButtonElement).disabled).toBe(true);
    expect(button.getAttribute("aria-busy")).toBe("true");
  });

  it("loading=false 时不渲染 spinner", () => {
    const { container } = render(() => <Button>普通按钮</Button>);
    expect(container.querySelector(".btn-spinner")).toBeNull();
  });
});

// button.css 文本回归:spinner 样式/动画自包含于本文件,
// bevel 阴影统一走 tokens.css 材质 token(禁止 rgba 字面量回潮)。
describe("button.css spinner 与 bevel token", () => {
  // vitest 的 cwd = frontend
  const css = readFileSync(
    join(process.cwd(), "src/styles/components/button.css"),
    "utf-8",
  );

  it("包含 .btn-spinner 样式与 @keyframes btn-spin", () => {
    expect(css).toContain(".btn-spinner");
    expect(css).toContain("@keyframes btn-spin");
  });

  it("box-shadow 使用 bevel 材质 token", () => {
    expect(css).toContain("var(--shadow-inset-bevel)");
    expect(css).toContain("var(--shadow-raised)");
    expect(css).toContain("var(--shadow-press)");
  });
});
