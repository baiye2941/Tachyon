import { describe, it, expect, afterEach, beforeEach } from "vitest";
import { render, cleanup, waitFor } from "@solidjs/testing-library";
import Announcer from "../Announcer";
import { addToast } from "../../stores/toast";
import { addToast as addRichToast, removeToast, getToasts } from "../ToastContainer";

describe("Announcer", () => {
  beforeEach(() => {
    // 清理已有 toast，避免测试间互相干扰
    getToasts().forEach((t) => removeToast(t.id));
  });

  afterEach(() => {
    cleanup();
  });

  it("应渲染全局 aria-live 区域", () => {
    const { container } = render(() => <Announcer />);
    const liveRegion = container.querySelector('[aria-live="polite"]');
    expect(liveRegion).not.toBeNull();
    expect(liveRegion?.classList.contains("sr-only")).toBe(true);
    expect(liveRegion?.getAttribute("role")).toBe("status");
    expect(liveRegion?.getAttribute("aria-atomic")).toBe("true");
  });

  it("Toast 触发后 live region 应更新为最新通知文本", async () => {
    const { container } = render(() => <Announcer />);

    addToast("下载完成", "success");

    await waitFor(() => {
      expect(container.textContent).toContain("下载完成");
    });
  });

  it("带描述的 Toast 应合并 title 与 description 播报", async () => {
    const { container } = render(() => <Announcer />);

    addRichToast({
      type: "error",
      title: "创建任务失败",
      description: "请检查网络连接",
    });

    await waitFor(() => {
      const text = container.textContent;
      expect(text).toContain("创建任务失败");
      expect(text).toContain("请检查网络连接");
    });
  });

  it("连续触发 Toast 只播报最新一条", async () => {
    const { container } = render(() => <Announcer />);

    addToast("第一条", "info");
    addToast("第二条", "info");

    await waitFor(() => {
      const text = container.textContent;
      expect(text).toContain("第二条");
      expect(text).not.toContain("第一条");
    });
  });
});
