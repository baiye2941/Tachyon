import { describe, it, expect, vi, afterEach } from "vitest";
import { render, fireEvent, cleanup } from "@solidjs/testing-library";
import ConfirmDialog from "../ConfirmDialog";

function waitForRaf() {
  return new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
}

describe("ConfirmDialog", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("open=false 时不渲染对话框", () => {
    render(() => (
      <ConfirmDialog
        open={false}
        title="标题"
        message="描述"
        onConfirm={vi.fn()}
        onCancel={vi.fn()}
      />
    ));
    expect(document.body.querySelector('[role="alertdialog"]')).toBeNull();
  });

  it("open=true 时渲染标题、描述和按钮", async () => {
    render(() => (
      <ConfirmDialog
        open={true}
        title="确认删除"
        message="确定要删除该任务吗?"
        onConfirm={vi.fn()}
        onCancel={vi.fn()}
      />
    ));
    await waitForRaf();

    const dialog = document.body.querySelector('[role="alertdialog"]');
    expect(dialog).not.toBeNull();
    expect(document.body.textContent).toContain("确认删除");
    expect(document.body.textContent).toContain("确定要删除该任务吗?");
    expect(document.body.textContent).toContain("确认");
    expect(document.body.textContent).toContain("取消");
  });

  it("点击确认按钮触发 onConfirm", async () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(() => (
      <ConfirmDialog
        open={true}
        title="标题"
        message="描述"
        onConfirm={onConfirm}
        onCancel={onCancel}
      />
    ));
    await waitForRaf();

    const confirmBtn = document.body.querySelector("button.btn-primary")!;
    fireEvent.click(confirmBtn);
    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(onConfirm).toHaveBeenCalledWith({ deleteLocalFile: false });
    expect(onCancel).not.toHaveBeenCalled();
  });

  it("点击取消按钮触发 onCancel", async () => {
    const onCancel = vi.fn();
    render(() => (
      <ConfirmDialog
        open={true}
        title="标题"
        message="描述"
        onConfirm={vi.fn()}
        onCancel={onCancel}
      />
    ));
    await waitForRaf();

    const cancelBtn = document.body.querySelector("button.btn-secondary")!;
    fireEvent.click(cancelBtn);
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("点击遮罩层触发 onCancel", async () => {
    const onCancel = vi.fn();
    render(() => (
      <ConfirmDialog
        open={true}
        title="标题"
        message="描述"
        onConfirm={vi.fn()}
        onCancel={onCancel}
      />
    ));
    await waitForRaf();

    const overlay = document.body.querySelector('[data-testid="dialog-overlay"]')!;
    fireEvent.click(overlay);
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("按下 Escape 触发 onCancel", async () => {
    const onCancel = vi.fn();
    render(() => (
      <ConfirmDialog
        open={true}
        title="标题"
        message="描述"
        onConfirm={vi.fn()}
        onCancel={onCancel}
      />
    ));
    await waitForRaf();

    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("tone=danger 时渲染 danger 确认按钮", async () => {
    const onConfirm = vi.fn();
    render(() => (
      <ConfirmDialog
        open={true}
        title="删除"
        message="不可撤销"
        tone="danger"
        onConfirm={onConfirm}
        onCancel={vi.fn()}
      />
    ));
    await waitForRaf();

    const dangerBtn = document.body.querySelector("button.btn-danger")!;
    expect(dangerBtn).not.toBeNull();
    fireEvent.click(dangerBtn);
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it("showDeleteLocalFileOption 渲染复选框并传递选中状态", async () => {
    const onConfirm = vi.fn();
    render(() => (
      <ConfirmDialog
        open={true}
        title="标题"
        message="描述"
        showDeleteLocalFileOption
        onConfirm={onConfirm}
        onCancel={vi.fn()}
      />
    ));
    await waitForRaf();

    const checkbox = document.body.querySelector('input[type="checkbox"]') as HTMLInputElement;
    expect(checkbox).not.toBeNull();
    expect(checkbox.checked).toBe(false);

    fireEvent.click(checkbox);
    expect(checkbox.checked).toBe(true);

    const confirmBtn = document.body.querySelector("button.btn-primary")!;
    fireEvent.click(confirmBtn);
    expect(onConfirm).toHaveBeenCalledWith({ deleteLocalFile: true });
  });

  it("保持 aria-labelledby 与 aria-describedby 关联", async () => {
    render(() => (
      <ConfirmDialog
        open={true}
        title="标题"
        message="描述"
        onConfirm={vi.fn()}
        onCancel={vi.fn()}
      />
    ));
    await waitForRaf();

    const dialog = document.body.querySelector('[role="alertdialog"]')!;
    expect(dialog.getAttribute("aria-labelledby")).toBe("confirm-dialog-title");
    expect(dialog.getAttribute("aria-describedby")).toBe("confirm-dialog-desc");
    expect(document.body.querySelector("#confirm-dialog-title")).not.toBeNull();
    expect(document.body.querySelector("#confirm-dialog-desc")).not.toBeNull();
  });

  it("tone=danger 时渲染 danger 确认按钮且不影响确认按钮", async () => {
    render(() => (
      <ConfirmDialog
        open={true}
        title="完成"
        message="操作成功"
        tone="danger"
        onConfirm={vi.fn()}
        onCancel={vi.fn()}
      />
    ));
    await waitForRaf();

    expect(document.body.querySelector(".confirm-dialog-icon--danger")).not.toBeNull();
    expect(document.body.querySelector("button.btn-danger")).not.toBeNull();
  });
});
