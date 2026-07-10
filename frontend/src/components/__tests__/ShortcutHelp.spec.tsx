import { describe, it, expect, afterEach, beforeEach, vi } from "vitest";
import { render, cleanup, fireEvent } from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../i18n";
import ShortcutHelp from "../ShortcutHelp";
import { setShortcut, resetAllShortcuts } from "../../stores/shortcuts";
import type { JSX } from "solid-js";

const renderWithI18n = (ui: () => JSX.Element) =>
  render(() => <I18nProvider i18n={i18n}>{ui()}</I18nProvider>);

describe("ShortcutHelp", () => {
  beforeEach(() => {
    localStorage.clear();
    resetAllShortcuts();
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    resetAllShortcuts();
    localStorage.clear();
  });

  it("默认渲染全部快捷键分组", () => {
    const { container } = renderWithI18n(() => (
      <ShortcutHelp visible={true} onClose={() => undefined} />
    ));

    expect(container.textContent).toContain("全局");
    expect(container.textContent).toContain("导航");
    expect(container.textContent).toContain("任务");
    expect(container.textContent).toContain("列表");
    expect(container.textContent).toContain("Ctrl");
    expect(container.textContent).toContain("K");
  });

  it("自定义绑定后同步更新", () => {
    setShortcut("shortcut.openCommandPalette", ["Ctrl", "Shift", "X"]);

    const { container } = renderWithI18n(() => (
      <ShortcutHelp visible={true} onClose={() => undefined} />
    ));

    const kbdElements = Array.from(container.querySelectorAll("kbd")).map(
      (el) => el.textContent,
    );
    expect(kbdElements).toContain("Shift");
    expect(kbdElements).toContain("X");
  });

  it("macOS 下 Ctrl 显示为 Cmd", () => {
    vi.stubGlobal("navigator", { platform: "MacIntel" });

    const { container } = renderWithI18n(() => (
      <ShortcutHelp visible={true} onClose={() => undefined} />
    ));

    const kbdElements = Array.from(container.querySelectorAll("kbd")).map(
      (el) => el.textContent,
    );
    expect(kbdElements).toContain("Cmd");
  });

  it("点击遮罩触发 onClose", () => {
    const onClose = vi.fn();
    const { container } = renderWithI18n(() => (
      <ShortcutHelp visible={true} onClose={onClose} />
    ));

    const overlay = container.querySelector(".panel-overlay");
    expect(overlay).not.toBeNull();
    fireEvent.click(overlay!);
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
