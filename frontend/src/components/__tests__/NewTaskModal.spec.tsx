import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@solidjs/testing-library";
import NewTaskModal from "../NewTaskModal";

vi.mock("../../api/invoke", () => ({
  api: {
    createTask: vi.fn(),
    pauseTask: vi.fn(),
  },
}));

vi.mock("../../stores/toast", () => ({
  addToast: vi.fn(),
}));

vi.mock("../../stores/downloads", () => ({
  refreshTaskList: vi.fn(),
}));

describe("NewTaskModal", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("磁力链接 dn 为空时展示 info hash 作为默认文件名", async () => {
    render(() => <NewTaskModal onClose={() => {}} />);

    const urlInput = screen.getByLabelText(/下载链接/) as HTMLTextAreaElement;
    fireEvent.input(urlInput, {
      target: {
        value:
          "magnet:?xt=urn:btih:WFL25E2HOBS656ZRTF7JX3HWFWVCURZ5&dn=&tr=http%3A%2F%2Ftracker.example.com%2Fannounce",
      },
      currentTarget: {
        value:
          "magnet:?xt=urn:btih:WFL25E2HOBS656ZRTF7JX3HWFWVCURZ5&dn=&tr=http%3A%2F%2Ftracker.example.com%2Fannounce",
      },
    });

    fireEvent.click(screen.getByRole("button", { name: "高级选项" }));

    const fileNameInput = await screen.findByLabelText("重命名(可选)");
    expect(fileNameInput).toHaveAttribute(
      "placeholder",
      "magnet-WFL25E2HOBS656ZRTF7JX3HWFWVCURZ5",
    );
  });

  it("Ctrl+A 在文本输入框内保留浏览器默认全选行为", () => {
    render(() => <NewTaskModal onClose={() => {}} />);

    const urlInput = screen.getByLabelText(/下载链接/) as HTMLTextAreaElement;
    const event = new KeyboardEvent("keydown", {
      key: "a",
      ctrlKey: true,
      bubbles: true,
      cancelable: true,
    });

    urlInput.dispatchEvent(event);

    expect(event.defaultPrevented).toBe(false);
  });
});
