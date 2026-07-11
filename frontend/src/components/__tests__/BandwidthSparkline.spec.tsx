import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  render,
  cleanup,
  screen,
  fireEvent,
} from "@solidjs/testing-library";
import { I18nProvider, i18n } from "../../i18n";
import {
  clearTaskHistory,
  pushTaskSpeed,
} from "../../stores/taskSpeedHistory";

const mockIsSmallScreen = vi.hoisted(() => vi.fn(() => false));

vi.mock("../../hooks/useMediaQuery", () => ({
  useIsSmallScreen: () => mockIsSmallScreen,
}));

vi.mock("../../hooks/useReducedMotion", () => ({
  useReducedMotion: () => () => true,
}));

async function renderSparkline(taskId: string, status = "downloading") {
  const { default: BandwidthSparkline } = await import("../BandwidthSparkline");
  return render(() => (
    <I18nProvider i18n={i18n}>
      <BandwidthSparkline taskId={taskId} status={status as never} />
    </I18nProvider>
  ));
}

describe("BandwidthSparkline", () => {
  beforeEach(() => {
    clearTaskHistory("task-1");
    mockIsSmallScreen.mockReturnValue(false);
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("应渲染标题和折叠按钮", async () => {
    await renderSparkline("task-1");

    expect(screen.getByText("速度趋势")).toBeTruthy();
    expect(screen.getByRole("button")).toBeTruthy();
    expect(screen.getByRole("button")?.getAttribute("aria-expanded")).toBe(
      "true",
    );
  });

  it("空数据时显示等待提示且不渲染曲线 SVG", async () => {
    await renderSparkline("task-1");

    expect(screen.getByText("等待速度数据...")).toBeTruthy();
    expect(document.querySelector(".bandwidth-sparkline-body svg")).toBeNull();
  });

  it("有数据时绘制 SVG 路径/区域", async () => {
    pushTaskSpeed("task-1", 100);
    pushTaskSpeed("task-1", 200);
    pushTaskSpeed("task-1", 150);

    await renderSparkline("task-1");

    const svg = document.querySelector(".bandwidth-sparkline-body svg");
    expect(svg).toBeTruthy();

    const paths = svg!.querySelectorAll("path");
    expect(paths.length).toBeGreaterThanOrEqual(2);

    const d = paths[0]!.getAttribute("d");
    expect(d).toBeTruthy();
    expect(d).not.toBe("");
  });

  it("小屏下默认折叠并隐藏图表", async () => {
    mockIsSmallScreen.mockReturnValue(true);

    await renderSparkline("task-1");

    expect(
      document.querySelector(".bandwidth-sparkline--collapsed"),
    ).toBeTruthy();
    expect(
      document.querySelector(".bandwidth-sparkline-body svg"),
    ).toBeNull();
    expect(screen.getByRole("button")?.getAttribute("aria-expanded")).toBe(
      "false",
    );
  });

  it("点击切换按钮可展开/折叠", async () => {
    await renderSparkline("task-1");

    const toggle = screen.getByRole("button");
    expect(
      document.querySelector(".bandwidth-sparkline--collapsed"),
    ).toBeNull();

    fireEvent.click(toggle);
    expect(
      document.querySelector(".bandwidth-sparkline--collapsed"),
    ).toBeTruthy();
    expect(toggle.getAttribute("aria-expanded")).toBe("false");

    fireEvent.click(toggle);
    expect(
      document.querySelector(".bandwidth-sparkline--collapsed"),
    ).toBeNull();
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
  });

  it("任务非下载态时不启动轮询但仍可渲染", async () => {
    pushTaskSpeed("task-1", 300);
    pushTaskSpeed("task-1", 400);

    await renderSparkline("task-1", "paused");

    expect(document.querySelector(".bandwidth-sparkline-body svg")).toBeTruthy();
  });
});
