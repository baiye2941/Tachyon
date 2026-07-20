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
    downloadUrl: "https://example.com/video.mp4",
    name: "video.mp4",
    type: "video",
    size: 1024 * 1024,
    discoveredAt: Date.now(),
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

  it("清空按钮直接清空资源(低风险操作,不弹确认框)", async () => {
    const onClearResources = vi.fn();
    renderPanel({ onClearResources });

    fireEvent.click(screen.getByTitle("清空"));
    // UX 审计:嗅探列表可重新嗅探/添加,清空不再弹二次确认
    expect(mockRequestConfirm).not.toHaveBeenCalled();
    await vi.waitFor(() => {
      expect(onClearResources).toHaveBeenCalledTimes(1);
    });
  });

  it("无资源时清空按钮不调用 onClearResources", async () => {
    const onClearResources = vi.fn();
    renderPanel({ onClearResources, resources: [] });

    fireEvent.click(screen.getByTitle("清空"));
    expect(onClearResources).not.toHaveBeenCalled();
    expect(mockRequestConfirm).not.toHaveBeenCalled();
  });
});
