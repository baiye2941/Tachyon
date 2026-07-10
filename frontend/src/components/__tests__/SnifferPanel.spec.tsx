import { describe, it, expect, afterEach, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../i18n";
import type { SnifferResource, CaptureConfig } from "../../types";
import SnifferPanel from "../SnifferPanel";

const mockAddToast = vi.fn();
const mockRequestConfirm = vi.fn();

vi.mock("../../stores/toast", () => ({
  addToast: (message: string, type?: string) => mockAddToast(message, type),
}));

vi.mock("../../stores/confirm", () => ({
  requestConfirm: (...args: unknown[]) => mockRequestConfirm(...args),
}));

function makeResource(overrides: Partial<SnifferResource> = {}): SnifferResource {
  return {
    id: "r1",
    url: "https://example.com/video.mp4",
    type: "video",
    size: 1024 * 1024,
    ...overrides,
  };
}

const defaultConfig: CaptureConfig = {
  enabledTypes: ["video", "audio", "document"],
  minSize: 0,
  urlFilters: [],
};

const renderPanel = (props: Record<string, unknown> = {}) => {
  return render(() => (
    <I18nProvider i18n={i18n}>
      <SnifferPanel
        visible={true}
        resources={[makeResource()]}
        captureConfig={defaultConfig}
        onClose={() => {}}
        onAddDownload={() => {}}
        onAddResource={() => {}}
        onClearResources={() => {}}
        onUpdateConfig={() => {}}
        {...props}
      />
    </I18nProvider>
  ));
};

describe("SnifferPanel", () => {
  beforeEach(() => {
    mockAddToast.mockReset();
    mockRequestConfirm.mockReset();
  });

  afterEach(() => {
    cleanup();
  });

  it("无效 URL 提交时使用 Toast 替代 window.alert", () => {
    renderPanel();
    const input = screen.getByPlaceholderText("粘贴资源 URL,回车添加");
    fireEvent.input(input, { target: { value: "not-a-url" } });
    fireEvent.keyDown(input, { key: "Enter" });

    expect(mockAddToast).toHaveBeenCalledTimes(1);
    expect(mockAddToast).toHaveBeenCalledWith(
      "请输入有效的 http(s) URL",
      "warning",
    );
  });

  it("清空按钮调用 requestConfirm 替代 window.confirm", async () => {
    mockRequestConfirm.mockResolvedValue({ ok: true, deleteLocalFile: false });
    const onClearResources = vi.fn();
    renderPanel({ onClearResources });

    fireEvent.click(screen.getByTitle("清空"));
    await vi.waitFor(() => {
      expect(mockRequestConfirm).toHaveBeenCalledTimes(1);
    });
    expect(mockRequestConfirm).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "清空嗅探资源",
        confirmLabel: "清空",
        tone: "danger",
      }),
    );
    await vi.waitFor(() => {
      expect(onClearResources).toHaveBeenCalledTimes(1);
    });
  });

  it("取消清空时不调用 onClearResources", async () => {
    mockRequestConfirm.mockResolvedValue({ ok: false, deleteLocalFile: false });
    const onClearResources = vi.fn();
    renderPanel({ onClearResources });

    fireEvent.click(screen.getByTitle("清空"));
    await vi.waitFor(() => {
      expect(mockRequestConfirm).toHaveBeenCalledTimes(1);
    });
    expect(onClearResources).not.toHaveBeenCalled();
  });
});
