import { describe, it, expect, vi } from "vitest";
import { createRoot, createSignal } from "solid-js";
import { useRafThrottle } from "../useRafThrottle";

const waitRaf = () =>
  new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));

describe("useRafThrottle", () => {
  it("应在 rAF 后执行一次回调", async () => {
    await createRoot(async (dispose) => {
      const cb = vi.fn();
      useRafThrottle({ source: 42, callback: cb });

      expect(cb).not.toHaveBeenCalled();
      await waitRaf();
      expect(cb).toHaveBeenCalledTimes(1);
      expect(cb).toHaveBeenCalledWith(42);

      dispose();
    });
  });

  it("同一帧内多次变化应只执行一次,取最新值", async () => {
    await createRoot(async (dispose) => {
      const cb = vi.fn();
      const [value, setValue] = createSignal(1);
      useRafThrottle({ source: () => value(), callback: cb });

      setValue(2);
      setValue(3);
      setValue(4);

      await waitRaf();
      expect(cb).toHaveBeenCalledTimes(1);
      expect(cb).toHaveBeenCalledWith(4);

      dispose();
    });
  });

  it("多帧变化应每帧执行一次", async () => {
    await createRoot(async (dispose) => {
      const cb = vi.fn();
      const [value, setValue] = createSignal(1);
      useRafThrottle({ source: () => value(), callback: cb });

      await waitRaf();
      expect(cb).toHaveBeenCalledWith(1);

      setValue(2);
      await waitRaf();
      expect(cb).toHaveBeenCalledWith(2);
      expect(cb).toHaveBeenCalledTimes(2);

      dispose();
    });
  });

  it("enabled=false 时不应执行回调", async () => {
    await createRoot(async (dispose) => {
      const cb = vi.fn();
      useRafThrottle({ source: 42, callback: cb, enabled: false });

      await waitRaf();
      expect(cb).not.toHaveBeenCalled();

      dispose();
    });
  });

  it("清理时应取消未执行的 rAF", async () => {
    await createRoot(async (dispose) => {
      const cb = vi.fn();
      useRafThrottle({ source: 42, callback: cb });

      dispose();
      await waitRaf();
      expect(cb).not.toHaveBeenCalled();
    });
  });
});
