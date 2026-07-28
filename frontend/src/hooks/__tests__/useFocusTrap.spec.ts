import { describe, it, expect, vi } from "vitest";
import { createRoot, createSignal } from "solid-js";
import { useFocusTrap, FOCUSABLE_SELECTOR } from "../useFocusTrap";

describe("useFocusTrap", () => {
  it("激活时应自动聚焦第一个可聚焦元素", () =>
    createRoot((dispose) => {
      const div = document.createElement("div");
      const btn1 = document.createElement("button");
      const btn2 = document.createElement("button");
      div.append(btn1, btn2);
      document.body.append(div);

      useFocusTrap({ active: true, container: div });

      return new Promise<void>((resolve) => {
        requestAnimationFrame(() => {
          requestAnimationFrame(() => {
            expect(document.activeElement).toBe(btn1);
            div.remove();
            dispose();
            resolve();
          });
        });
      });
    }));

  it("data-autofocus 元素优先于第一个可聚焦元素", () =>
    createRoot((dispose) => {
      const div = document.createElement("div");
      const btn1 = document.createElement("button");
      const btn2 = document.createElement("button");
      btn2.setAttribute("data-autofocus", "true");
      div.append(btn1, btn2);
      document.body.append(div);

      useFocusTrap({ active: true, container: div });

      return new Promise<void>((resolve) => {
        requestAnimationFrame(() => {
          requestAnimationFrame(() => {
            expect(document.activeElement).toBe(btn2);
            div.remove();
            dispose();
            resolve();
          });
        });
      });
    }));

  it("Tab 在最后一个元素时应循环到第一个", () =>
    createRoot((dispose) => {
      const div = document.createElement("div");
      const btn1 = document.createElement("button");
      const btn2 = document.createElement("button");
      div.append(btn1, btn2);
      document.body.append(div);

      useFocusTrap({ active: true, container: div });

      return new Promise<void>((resolve) => {
        requestAnimationFrame(() => {
          requestAnimationFrame(() => {
            btn2.focus();
            const event = new KeyboardEvent("keydown", {
              key: "Tab",
              bubbles: true,
            });
            const preventDefault = vi.spyOn(event, "preventDefault");
            document.dispatchEvent(event);
            expect(preventDefault).toHaveBeenCalled();
            expect(document.activeElement).toBe(btn1);
            div.remove();
            dispose();
            resolve();
          });
        });
      });
    }));

  it("Shift+Tab 在第一个元素时应循环到最后一个", () =>
    createRoot((dispose) => {
      const div = document.createElement("div");
      const btn1 = document.createElement("button");
      const btn2 = document.createElement("button");
      div.append(btn1, btn2);
      document.body.append(div);

      useFocusTrap({ active: true, container: div });

      return new Promise<void>((resolve) => {
        requestAnimationFrame(() => {
          requestAnimationFrame(() => {
            const event = new KeyboardEvent("keydown", {
              key: "Tab",
              shiftKey: true,
              bubbles: true,
            });
            const preventDefault = vi.spyOn(event, "preventDefault");
            document.dispatchEvent(event);
            expect(preventDefault).toHaveBeenCalled();
            expect(document.activeElement).toBe(btn2);
            div.remove();
            dispose();
            resolve();
          });
        });
      });
    }));

  it("Escape 触发 onEscape 回调", () =>
    createRoot((dispose) => {
      const div = document.createElement("div");
      const onEscape = vi.fn();
      div.append(document.createElement("button"));
      document.body.append(div);

      useFocusTrap({ active: true, container: div, onEscape });

      return new Promise<void>((resolve) => {
        requestAnimationFrame(() => {
          requestAnimationFrame(() => {
            const event = new KeyboardEvent("keydown", {
              key: "Escape",
              bubbles: true,
            });
            document.dispatchEvent(event);
            expect(onEscape).toHaveBeenCalledTimes(1);
            div.remove();
            dispose();
            resolve();
          });
        });
      });
    }));

  it("关闭时应恢复先前焦点", () =>
    createRoot((dispose) => {
      const outer = document.createElement("button");
      document.body.append(outer);
      outer.focus();

      const div = document.createElement("div");
      const btn = document.createElement("button");
      div.append(btn);
      document.body.append(div);

      const [active] = createSignal(true);
      useFocusTrap({ active: () => active(), container: div });

      return new Promise<void>((resolve) => {
        requestAnimationFrame(() => {
          requestAnimationFrame(() => {
            expect(document.activeElement).toBe(btn);
            // 直接调用 cleanup 模拟关闭
            dispose();
            // createEffect cleanup 应恢复 outer 焦点
            expect(document.activeElement).toBe(outer);
            div.remove();
            outer.remove();
            resolve();
          });
        });
      });
    }));

  it("未激活时不应监听键盘", () =>
    createRoot((dispose) => {
      const div = document.createElement("div");
      const onEscape = vi.fn();
      div.append(document.createElement("button"));
      document.body.append(div);

      useFocusTrap({ active: false, container: div, onEscape });

      const event = new KeyboardEvent("keydown", {
        key: "Escape",
        bubbles: true,
      });
      document.dispatchEvent(event);
      expect(onEscape).not.toHaveBeenCalled();

      div.remove();
      dispose();
    }));

  it("FOCUSABLE_SELECTOR 包含标准可聚焦元素", () => {
    expect(FOCUSABLE_SELECTOR).toContain("button");
    expect(FOCUSABLE_SELECTOR).toContain("input");
    expect(FOCUSABLE_SELECTOR).toContain('[tabindex]:not([tabindex="-1"])');
  });

  it("两层弹层叠加时 Escape 只触发栈顶 onEscape,栈顶卸载后触发下一层", () => {
    const outerDiv = document.createElement("div");
    outerDiv.append(document.createElement("button"));
    document.body.append(outerDiv);
    const innerDiv = document.createElement("div");
    innerDiv.append(document.createElement("button"));
    document.body.append(innerDiv);

    const onEscapeOuter = vi.fn();
    const onEscapeInner = vi.fn();

    const disposeOuter = createRoot((dispose) => {
      useFocusTrap({
        active: true,
        container: outerDiv,
        onEscape: onEscapeOuter,
      });
      return dispose;
    });
    const disposeInner = createRoot((dispose) => {
      useFocusTrap({
        active: true,
        container: innerDiv,
        onEscape: onEscapeInner,
      });
      return dispose;
    });

    // 栈顶(inner)响应,底层(outer)不响应,禁止 Esc 双关
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    );
    expect(onEscapeInner).toHaveBeenCalledTimes(1);
    expect(onEscapeOuter).not.toHaveBeenCalled();

    // 栈顶卸载后,下一层恢复响应
    disposeInner();
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    );
    expect(onEscapeOuter).toHaveBeenCalledTimes(1);

    disposeOuter();
    outerDiv.remove();
    innerDiv.remove();
  });

  it("叠加时 Tab 循环仅栈顶陷阱响应", () => {
    const outerDiv = document.createElement("div");
    const outerBtn1 = document.createElement("button");
    const outerBtn2 = document.createElement("button");
    outerDiv.append(outerBtn1, outerBtn2);
    document.body.append(outerDiv);
    const innerDiv = document.createElement("div");
    const innerBtn1 = document.createElement("button");
    const innerBtn2 = document.createElement("button");
    innerDiv.append(innerBtn1, innerBtn2);
    document.body.append(innerDiv);

    const disposeOuter = createRoot((dispose) => {
      useFocusTrap({ active: true, container: outerDiv });
      return dispose;
    });
    const disposeInner = createRoot((dispose) => {
      useFocusTrap({ active: true, container: innerDiv });
      return dispose;
    });

    // 焦点在底层容器末尾时,底层陷阱不得响应 Tab(栈顶优先)
    outerBtn2.focus();
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Tab", bubbles: true }),
    );
    expect(document.activeElement).toBe(outerBtn2);

    // 栈顶陷阱正常循环:末尾 Tab 回到首位
    innerBtn2.focus();
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Tab", bubbles: true }),
    );
    expect(document.activeElement).toBe(innerBtn1);

    disposeInner();
    disposeOuter();
    outerDiv.remove();
    innerDiv.remove();
  });
});
