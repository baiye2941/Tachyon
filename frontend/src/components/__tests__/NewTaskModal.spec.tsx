import { describe, it, expect, vi, afterEach, beforeEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@solidjs/testing-library";
import NewTaskModal from "../NewTaskModal";

function mockMatchMedia(matches: boolean) {
  const listeners: ((e: MediaQueryListEvent) => void)[] = [];
  const mql = {
    matches,
    media: "",
    onchange: null,
    addEventListener: (
      _type: string,
      listener: (e: MediaQueryListEvent) => void,
    ) => listeners.push(listener),
    removeEventListener: (
      _type: string,
      listener: (e: MediaQueryListEvent) => void,
    ) => {
      const i = listeners.indexOf(listener);
      if (i >= 0) listeners.splice(i, 1);
    },
    dispatchEvent: () => true,
    addListener: () => {},
    removeListener: () => {},
  };
  vi.stubGlobal("matchMedia", () => mql);
}

vi.mock("../../api/invoke", () => ({
  api: {
    createTask: vi.fn(),
    probeFilename: vi.fn(),
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
    expect((fileNameInput as HTMLInputElement).placeholder).toBe(
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

  describe("探测后自动填名", () => {
    it("探测成功后重命名 input value 填入探测名", async () => {
      const { api } = await import("../../api/invoke");
      (api.probeFilename as ReturnType<typeof vi.fn>).mockResolvedValue(
        "model.safetensors",
      );

      render(() => <NewTaskModal onClose={() => {}} />);

      // 输入单个有效 URL 触发 displayFilename 显示探测按钮
      const urlInput = screen.getByLabelText(/下载链接/) as HTMLTextAreaElement;
      fireEvent.input(urlInput, {
        target: { value: "https://example.com/model" },
        currentTarget: { value: "https://example.com/model" },
      });

      // 展开高级选项,重命名 input 出现
      fireEvent.click(screen.getByRole("button", { name: "高级选项" }));
      const fileNameInput = (await screen.findByLabelText(
        "重命名(可选)",
      )) as HTMLInputElement;

      // 点探测前 input value 为空
      expect(fileNameInput.value).toBe("");

      // 点探测
      const probeBtn = screen.getByRole("button", { name: /探测/ });
      await fireEvent.click(probeBtn);
      await screen.findByDisplayValue("model.safetensors");

      expect(fileNameInput.value).toBe("model.safetensors");
      expect(api.probeFilename).toHaveBeenCalledWith(
        "https://example.com/model",
      );
    });

    it("探测后用户可继续编辑 input", async () => {
      const { api } = await import("../../api/invoke");
      (api.probeFilename as ReturnType<typeof vi.fn>).mockResolvedValue(
        "model.safetensors",
      );

      render(() => <NewTaskModal onClose={() => {}} />);

      const urlInput = screen.getByLabelText(/下载链接/) as HTMLTextAreaElement;
      fireEvent.input(urlInput, {
        target: { value: "https://example.com/model" },
        currentTarget: { value: "https://example.com/model" },
      });

      fireEvent.click(screen.getByRole("button", { name: "高级选项" }));
      const fileNameInput = (await screen.findByLabelText(
        "重命名(可选)",
      )) as HTMLInputElement;

      const probeBtn = screen.getByRole("button", { name: /探测/ });
      await fireEvent.click(probeBtn);
      await screen.findByDisplayValue("model.safetensors");

      // 用户编辑
      fireEvent.input(fileNameInput, {
        target: { value: "model-renamed.safetensors" },
        currentTarget: { value: "model-renamed.safetensors" },
      });

      expect(fileNameInput.value).toBe("model-renamed.safetensors");
    });

    it("URL 变化后清空已填入的文件名", async () => {
      const { api } = await import("../../api/invoke");
      (api.probeFilename as ReturnType<typeof vi.fn>).mockResolvedValue(
        "model.safetensors",
      );

      render(() => <NewTaskModal onClose={() => {}} />);

      const urlInput = screen.getByLabelText(/下载链接/) as HTMLTextAreaElement;
      fireEvent.input(urlInput, {
        target: { value: "https://example.com/model" },
        currentTarget: { value: "https://example.com/model" },
      });

      fireEvent.click(screen.getByRole("button", { name: "高级选项" }));
      const fileNameInput = (await screen.findByLabelText(
        "重命名(可选)",
      )) as HTMLInputElement;

      const probeBtn = screen.getByRole("button", { name: /探测/ });
      await fireEvent.click(probeBtn);
      await screen.findByDisplayValue("model.safetensors");
      expect(fileNameInput.value).toBe("model.safetensors");

      // URL 变化
      fireEvent.input(urlInput, {
        target: { value: "https://example.com/other-file" },
        currentTarget: { value: "https://example.com/other-file" },
      });

      expect(fileNameInput.value).toBe("");
    });

    it("重新探测覆盖用户已输入的内容", async () => {
      const { api } = await import("../../api/invoke");
      (api.probeFilename as ReturnType<typeof vi.fn>)
        .mockResolvedValueOnce("old.bin")
        .mockResolvedValueOnce("new.bin");

      render(() => <NewTaskModal onClose={() => {}} />);

      const urlInput = screen.getByLabelText(/下载链接/) as HTMLTextAreaElement;
      fireEvent.input(urlInput, {
        target: { value: "https://example.com/file" },
        currentTarget: { value: "https://example.com/file" },
      });

      fireEvent.click(screen.getByRole("button", { name: "高级选项" }));
      const fileNameInput = (await screen.findByLabelText(
        "重命名(可选)",
      )) as HTMLInputElement;

      // 第一次探测填入 old.bin
      const probeBtn = screen.getByRole("button", { name: /探测/ });
      await fireEvent.click(probeBtn);
      await screen.findByDisplayValue("old.bin");
      expect(fileNameInput.value).toBe("old.bin");

      // 第二次探测覆盖为 new.bin
      await fireEvent.click(probeBtn);
      await screen.findByDisplayValue("new.bin");
      expect(fileNameInput.value).toBe("new.bin");
    });

    it("批量多 URL 时探测按钮禁用", async () => {
      render(() => <NewTaskModal onClose={() => {}} />);

      const urlInput = screen.getByLabelText(/下载链接/) as HTMLTextAreaElement;
      fireEvent.input(urlInput, {
        target: {
          value: "https://example.com/file1\nhttps://example.com/file2",
        },
        currentTarget: {
          value: "https://example.com/file1\nhttps://example.com/file2",
        },
      });

      // displayFilename 有值时探测按钮渲染;批量时(2 个)应 disabled
      const probeBtn = screen.getByRole("button", { name: /探测/ });
      expect((probeBtn as HTMLButtonElement).disabled).toBe(true);
    });

    it("T-1 探测填名后提交,探测名作为 file_name 传入后端", async () => {
      const { api } = await import("../../api/invoke");
      (api.probeFilename as ReturnType<typeof vi.fn>).mockResolvedValue(
        "model.safetensors",
      );
      (api.createTask as ReturnType<typeof vi.fn>).mockResolvedValue("task-1");

      render(() => <NewTaskModal onClose={() => {}} />);

      const urlInput = screen.getByLabelText(/下载链接/) as HTMLTextAreaElement;
      fireEvent.input(urlInput, {
        target: { value: "https://example.com/model" },
        currentTarget: { value: "https://example.com/model" },
      });

      fireEvent.click(screen.getByRole("button", { name: "高级选项" }));
      await screen.findByLabelText("重命名(可选)");

      // 点探测填名
      const probeBtn = screen.getByRole("button", { name: /探测/ });
      await fireEvent.click(probeBtn);
      await screen.findByDisplayValue("model.safetensors");

      // 点提交(主按钮文案为"开始下载"或含"开始",用 getByRole 取主按钮)
      const submitBtn = screen.getByRole("button", { name: /开始/ });
      await fireEvent.click(submitBtn);
      // createTask 是异步 Promise.allSettled,等一下
      await new Promise((r) => setTimeout(r, 50));

      // 断言 createTask 被调用,第 4 参数(文件名)为探测名
      expect(api.createTask).toHaveBeenCalled();
      const callArgs = (api.createTask as ReturnType<typeof vi.fn>).mock
        .calls[0]!;
      expect(callArgs[0]).toBe("https://example.com/model");
      expect(callArgs[3]).toBe("model.safetensors");
    });

    it("T-2 单 URL 时探测按钮可用(非 disabled)", async () => {
      const { api } = await import("../../api/invoke");
      (api.probeFilename as ReturnType<typeof vi.fn>).mockResolvedValue(
        "model.safetensors",
      );

      render(() => <NewTaskModal onClose={() => {}} />);

      const urlInput = screen.getByLabelText(/下载链接/) as HTMLTextAreaElement;
      fireEvent.input(urlInput, {
        target: { value: "https://example.com/model" },
        currentTarget: { value: "https://example.com/model" },
      });

      // 单 URL 时探测按钮应 enabled(与批量 disabled 对称)
      const probeBtn = screen.getByRole("button", { name: /探测/ });
      expect((probeBtn as HTMLButtonElement).disabled).toBe(false);
    });
  });

  describe("自动开始参数", () => {
    it("默认勾选自动开始时,createTask 第 5 参数为 true", async () => {
      const { api } = await import("../../api/invoke");
      (api.createTask as ReturnType<typeof vi.fn>).mockResolvedValue("task-1");

      render(() => <NewTaskModal onClose={() => {}} />);

      const urlInput = screen.getByLabelText(/下载链接/) as HTMLTextAreaElement;
      fireEvent.input(urlInput, {
        target: { value: "https://example.com/model" },
        currentTarget: { value: "https://example.com/model" },
      });

      const submitBtn = screen.getByRole("button", { name: /开始/ });
      await fireEvent.click(submitBtn);
      await new Promise((r) => setTimeout(r, 50));

      expect(api.createTask).toHaveBeenCalledTimes(1);
      const callArgs = (api.createTask as ReturnType<typeof vi.fn>).mock
        .calls[0]!;
      expect(callArgs[4]).toBe(true);
    });

    it("取消勾选自动开始时,createTask 第 5 参数为 false", async () => {
      const { api } = await import("../../api/invoke");
      (api.createTask as ReturnType<typeof vi.fn>).mockResolvedValue("task-1");

      render(() => <NewTaskModal onClose={() => {}} />);

      const urlInput = screen.getByLabelText(/下载链接/) as HTMLTextAreaElement;
      fireEvent.input(urlInput, {
        target: { value: "https://example.com/model" },
        currentTarget: { value: "https://example.com/model" },
      });

      const autoStartCheckbox = screen.getByRole("checkbox");
      fireEvent.click(autoStartCheckbox);

      const submitBtn = screen.getByRole("button", { name: /开始/ });
      await fireEvent.click(submitBtn);
      await new Promise((r) => setTimeout(r, 50));

      expect(api.createTask).toHaveBeenCalledTimes(1);
      const callArgs = (api.createTask as ReturnType<typeof vi.fn>).mock
        .calls[0]!;
      expect(callArgs[4]).toBe(false);
    });
  });

  describe("移动端窄屏适配", () => {
    beforeEach(() => {
      mockMatchMedia(true);
    });

    afterEach(() => {
      vi.unstubAllGlobals();
    });

    it("小屏下弹窗添加 new-task-modal--narrow 类并减少内边距", () => {
      const { container } = render(() => <NewTaskModal onClose={() => {}} />);

      const panel = container.querySelector(".panel-surface");
      expect(panel).toBeTruthy();
      expect(panel!.classList.contains("new-task-modal--narrow")).toBe(true);
      expect(panel!.getAttribute("style")).toContain("padding: 16px");
    });
  });
});
