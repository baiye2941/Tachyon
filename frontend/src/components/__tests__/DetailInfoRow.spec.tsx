import { describe, it, expect, afterEach } from "vitest";
import { render, fireEvent, cleanup } from "@solidjs/testing-library";
import DetailInfoRow from "../DetailInfoRow";

describe("DetailInfoRow 元数据行", () => {
  afterEach(() => {
    cleanup();
  });

  it("非 collapsible:不渲染折叠按钮,长值保持 break-all 平铺", () => {
    const { container } = render(() => (
      <DetailInfoRow label="下载链接" value="magnet:?xt=urn:btih:abc" />
    ));

    expect(container.querySelector(".detail-disclosure-btn")).toBeNull();
    expect(container.querySelector(".detail-info-value--clamped")).toBeNull();
  });

  it("collapsible:默认单行省略,aria-expanded=false", () => {
    const { container } = render(() => (
      <DetailInfoRow label="下载链接" value="magnet:?xt=urn:btih:abc" collapsible />
    ));

    const btn = container.querySelector(".detail-disclosure-btn");
    expect(btn).not.toBeNull();
    expect(btn?.getAttribute("aria-expanded")).toBe("false");
    expect(container.querySelector(".detail-info-value--clamped")).not.toBeNull();
  });

  it("collapsible:点击 chevron 展开完整值,再次点击收起", () => {
    const { container } = render(() => (
      <DetailInfoRow label="下载链接" value="magnet:?xt=urn:btih:abc" collapsible />
    ));
    const btn = container.querySelector(".detail-disclosure-btn")!;

    fireEvent.click(btn);
    expect(btn.getAttribute("aria-expanded")).toBe("true");
    expect(container.querySelector(".detail-info-value--clamped")).toBeNull();

    fireEvent.click(btn);
    expect(btn.getAttribute("aria-expanded")).toBe("false");
    expect(container.querySelector(".detail-info-value--clamped")).not.toBeNull();
  });

  it("collapsible 与 copyable 可同时存在", () => {
    const { container } = render(() => (
      <DetailInfoRow
        label="下载链接"
        value="magnet:?xt=urn:btih:abc"
        collapsible
        copyable
        onCopy={() => {}}
      />
    ));

    expect(container.querySelector(".detail-disclosure-btn")).not.toBeNull();
    expect(container.querySelector(".icon-btn-sm")).not.toBeNull();
  });
});
