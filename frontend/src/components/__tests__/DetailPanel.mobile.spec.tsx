import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, cleanup, screen } from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../i18n";
import type { TaskInfo } from "../../types";

const mockApi = vi.hoisted(() => ({
  pauseTask: vi.fn(),
  resumeTask: vi.fn(),
  cancelTask: vi.fn(),
  deleteTask: vi.fn(),
  createTask: vi.fn(),
  openFolder: vi.fn(),
}));

vi.mock("../../api/invoke", () => ({
  api: mockApi,
}));

vi.mock("../../stores/downloads", () => ({
  refreshTaskList: vi.fn(),
}));

vi.mock("../../stores/taskFragments", () => ({
  loadTaskFragments: vi.fn(),
  clearTaskFragments: vi.fn(),
  getTaskFragmentData: vi.fn(() => undefined),
}));

vi.mock("../../stores/toast", () => ({
  addToast: vi.fn(),
}));

vi.mock("../../stores/confirm", () => ({
  requestConfirm: vi.fn(() =>
    Promise.resolve({ ok: true, deleteLocalFile: false }),
  ),
}));

vi.mock("../../stores/taskSpeedHistory", () => ({
  clearTaskHistory: vi.fn(),
}));

vi.mock("../../hooks/useReducedMotion", () => ({
  useReducedMotion: () => () => true,
}));

vi.mock("../../hooks/useMediaQuery", () => ({
  useIsNarrowScreen: () => () => true,
  useIsSmallScreen: () => () => true,
}));

vi.mock("../../hooks/useFocusTrap", () => ({
  useFocusTrap: () => {},
}));

vi.mock("@motionone/solid", () => ({
  Motion: {
    div: (props: Record<string, unknown>) => <div {...props} />,
  },
}));

vi.mock("../SpeedChart", () => ({
  default: () => <div data-testid="speed-chart">SpeedChart</div>,
}));

vi.mock("../ChunkMatrix", () => ({
  default: () => <div data-testid="chunk-matrix">ChunkMatrix</div>,
}));

vi.mock("../AnimatedNumber", () => ({
  default: (props: { value: string }) => <span>{props.value}</span>,
}));

const baseTask: TaskInfo = {
  id: "task-1",
  url: "https://example.com/model.gguf",
  fileName: "model.gguf",
  fileSize: 1024 * 1024 * 100,
  downloaded: 1024 * 1024 * 50,
  progress: 0.5,
  speed: 1024 * 1024,
  status: "downloading",
  fragmentsTotal: 8,
  fragmentsDone: 4,
  createdAt: "2026-06-25T08:00:00.000Z",
  savePath: "D:\\Downloads\\model.gguf",
  activeConcurrency: 4,
};

function waitForRaf() {
  return new Promise<void>((resolve) => {
    requestAnimationFrame(() => {
      requestAnimationFrame(() => resolve());
    });
  });
}

describe("DetailPanel 移动端窄屏适配", () => {
  beforeEach(() => {
    localStorage.clear();
    Object.assign(navigator, {
      clipboard: {
        writeText: vi.fn(),
      },
    });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("覆盖式面板在窄屏下占满宽度并添加 narrow 类", async () => {
    const { default: DetailPanel } = await import("../DetailPanel");
    render(() => (
      <I18nProvider i18n={i18n}>
        <DetailPanel task={baseTask} onClose={() => {}} variant="overlay" />
      </I18nProvider>
    ));
    await waitForRaf();

    const panel = document.querySelector(".detail-panel");
    expect(panel).toBeTruthy();
    expect(panel!.classList.contains("detail-panel--narrow")).toBe(true);
  });

  it("窄屏下关闭按钮使用更大的 icon 尺寸", async () => {
    const { default: DetailPanel } = await import("../DetailPanel");
    render(() => (
      <I18nProvider i18n={i18n}>
        <DetailPanel task={baseTask} onClose={() => {}} variant="overlay" />
      </I18nProvider>
    ));
    await waitForRaf();

    const closeBtns = screen.getAllByRole("button", { name: "关闭详情" });
    expect(closeBtns.length).toBeGreaterThan(0);
    closeBtns.forEach((btn) => {
      expect(btn.className).toContain("icon-btn");
      expect(btn.className).not.toContain("icon-btn-sm");
    });
  });
});
