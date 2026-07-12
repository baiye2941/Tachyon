import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  render,
  cleanup,
  fireEvent,
  screen,
} from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../i18n";
import type { TaskInfo } from "../../types";
import DetailPanel from "../DetailPanel";
import { refreshTaskList } from "../../stores/downloads";

const mockApi = vi.hoisted(() => ({
  pauseTask: vi.fn(),
  resumeTask: vi.fn(),
  cancelTask: vi.fn(),
  deleteTask: vi.fn(),
  createTask: vi.fn(),
  openFolder: vi.fn(),
  addTaskTag: vi.fn(),
  removeTaskTag: vi.fn(),
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
  useIsNarrowScreen: () => () => false,
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

function renderWithI18n(task: TaskInfo | null, onClose = () => {}) {
  return render(() => (
    <I18nProvider i18n={i18n}>
      <DetailPanel task={task} onClose={onClose} />
    </I18nProvider>
  ));
}

describe("DetailPanel", () => {
  beforeEach(() => {
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

  it("应渲染文件名、大百分比进度和状态徽标", async () => {
    renderWithI18n(baseTask);
    await waitForRaf();

    const text = document.body.textContent;
    expect(text).toContain("model.gguf");
    expect(text).toContain("50.0%");
    expect(text).toContain("下载中");
  });

  it("活动指标应显示真实并发分片数而非占位符", async () => {
    renderWithI18n(baseTask);
    await waitForRaf();

    const cards = document.querySelectorAll(".metric-card");
    // 仅保留 2 个指标卡:剩余时间 + 并发分片
    expect(cards.length).toBe(2);

    const text = document.body.textContent;
    expect(text).toContain("并发分片");
    expect(text).toContain("4");
    expect(text).not.toContain("线程");
  });

  it("并发分片为 0 时应显示占位符", async () => {
    renderWithI18n({ ...baseTask, activeConcurrency: 0 });
    await waitForRaf();

    const cards = Array.from(document.querySelectorAll(".metric-card"));
    const concurrencyCard = cards.find((c) =>
      c.textContent?.includes("并发分片"),
    );
    expect(concurrencyCard?.textContent).toContain("—");
  });

  it("下载中任务底部应显示暂停按钮", async () => {
    renderWithI18n(baseTask);
    await waitForRaf();

    const pauseBtn = screen.getByRole("button", { name: /暂停下载/ });
    expect(pauseBtn).toBeTruthy();

    fireEvent.click(pauseBtn);
    expect(mockApi.pauseTask).toHaveBeenCalledWith("task-1");
  });

  it("已暂停任务底部应显示恢复按钮", async () => {
    renderWithI18n({ ...baseTask, status: "paused", speed: 0 });
    await waitForRaf();

    const resumeBtn = screen.getByRole("button", { name: /恢复下载/ });
    expect(resumeBtn).toBeTruthy();

    fireEvent.click(resumeBtn);
    expect(mockApi.resumeTask).toHaveBeenCalledWith("task-1");
  });

  it("头部快捷操作应包含复制链接、打开文件夹、重新下载", async () => {
    renderWithI18n(baseTask);
    await waitForRaf();

    expect(screen.getByRole("button", { name: "复制链接" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "打开文件夹" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "重新下载" })).toBeTruthy();
  });

  it("点击复制链接应写入剪贴板", async () => {
    renderWithI18n(baseTask);
    await waitForRaf();

    fireEvent.click(screen.getByRole("button", { name: "复制链接" }));

    expect(navigator.clipboard.writeText).toHaveBeenCalledWith(
      "https://example.com/model.gguf",
    );
  });

  it("点击打开文件夹应调用 api.openFolder", async () => {
    renderWithI18n(baseTask);
    await waitForRaf();

    fireEvent.click(screen.getByRole("button", { name: "打开文件夹" }));

    expect(mockApi.openFolder).toHaveBeenCalledWith(
      "D:\\Downloads\\model.gguf",
    );
  });

  it("无保存路径时不显示打开文件夹按钮", async () => {
    renderWithI18n({ ...baseTask, savePath: "" });
    await waitForRaf();

    expect(
      screen.queryByRole("button", { name: "打开文件夹" }),
    ).toBeNull();
  });

  it("点击重新下载应创建新任务", async () => {
    mockApi.createTask.mockResolvedValue("task-2");
    renderWithI18n(baseTask);
    await waitForRaf();

    fireEvent.click(screen.getByRole("button", { name: "重新下载" }));
    await new Promise((r) => setTimeout(r, 0));

    expect(mockApi.createTask).toHaveBeenCalledWith(
      "https://example.com/model.gguf",
    );
  });

  it("失败任务应显示可展开的诊断信息", async () => {
    const failedTask: TaskInfo = {
      ...baseTask,
      status: "failed",
      speed: 0,
      errorReason: "connection timeout",
    };
    renderWithI18n(failedTask);
    await waitForRaf();

    expect(screen.getByRole("alert")).toBeTruthy();
    expect(document.body.textContent).toContain("连接超时");

    const toggle = screen.getByRole("button", { name: /展开诊断/ });
    fireEvent.click(toggle);

    expect(document.body.textContent).toContain("connection timeout");
  });

  it("URL 和保存路径默认可见", async () => {
    renderWithI18n(baseTask);
    await waitForRaf();

    expect(screen.getByText("下载链接")).toBeTruthy();
    expect(screen.getByText("https://example.com/model.gguf")).toBeTruthy();
    expect(screen.getByText("保存路径")).toBeTruthy();
    expect(screen.getByText("D:\\Downloads\\model.gguf")).toBeTruthy();
  });

  it("删除任务应弹出确认并调用 api.deleteTask", async () => {
    const { requestConfirm } = await import("../../stores/confirm");
    renderWithI18n(baseTask);
    await waitForRaf();

    fireEvent.click(screen.getByRole("button", { name: "删除任务" }));
    await new Promise((r) => setTimeout(r, 0));

    expect(requestConfirm).toHaveBeenCalled();
    expect(mockApi.deleteTask).toHaveBeenCalledWith("task-1", {
      skipConfirm: true,
      deleteLocalFile: false,
    });
  });

  it("关闭按钮应触发 onClose", async () => {
    const onClose = vi.fn();
    renderWithI18n(baseTask, onClose);
    await waitForRaf();

    const closeBtns = screen.getAllByRole("button", { name: "关闭详情" });
    fireEvent.click(closeBtns[0]!);

    // 关闭有过渡动画,等待 350ms 后断言回调
    await new Promise((r) => setTimeout(r, 350));
    expect(onClose).toHaveBeenCalled();
  });

  it("应渲染任务标签", async () => {
    renderWithI18n({ ...baseTask, tags: ["ai", "model"] });
    await waitForRaf();

    expect(screen.getByText("标签")).toBeTruthy();
    expect(screen.getByText("ai")).toBeTruthy();
    expect(screen.getByText("model")).toBeTruthy();
  });

  it("输入标签并回车应调用 api.addTaskTag 并刷新列表", async () => {
    renderWithI18n(baseTask);
    await waitForRaf();

    const input = screen.getByPlaceholderText("输入标签,回车添加");
    fireEvent.input(input, { target: { value: "ai" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await new Promise((r) => setTimeout(r, 0));
    expect(mockApi.addTaskTag).toHaveBeenCalledWith("task-1", "ai");
    expect(refreshTaskList).toHaveBeenCalled();
  });

  it("点击标签移除按钮应调用 api.removeTaskTag 并刷新列表", async () => {
    renderWithI18n({ ...baseTask, tags: ["ai", "model"] });
    await waitForRaf();

    fireEvent.click(screen.getByRole("button", { name: "移除标签 ai" }));

    await new Promise((r) => setTimeout(r, 0));
    expect(mockApi.removeTaskTag).toHaveBeenCalledWith("task-1", "ai");
    expect(refreshTaskList).toHaveBeenCalled();
  });
});
