import { describe, it, expect, vi, afterEach } from "vitest";
import { render, cleanup, fireEvent } from "@solidjs/testing-library";
import ContextMenu from "../ContextMenu";
import type { TaskInfo } from "../../types";

const mockTask: TaskInfo = {
  id: "task-1",
  fileName: "model.gguf",
  url: "https://example.com/model.gguf",
  fileSize: 1024000,
  downloaded: 512000,
  progress: 0.5,
  speed: 1048576,
  status: "downloading",
  fragmentsTotal: 4,
  fragmentsDone: 2,
  createdAt: "2026-06-16T00:00:00Z",
  savePath: "/downloads",
};

const completedTask: TaskInfo = { ...mockTask, status: "completed" };
const pausedTask: TaskInfo = { ...mockTask, status: "paused" };

describe("ContextMenu 可访问性", () => {
  afterEach(() => {
    cleanup();
  });

  it('应渲染 role="menu" 和 role="menuitem"', () => {
    const { container } = render(() => (
      <ContextMenu
        x={100}
        y={100}
        visible={true}
        task={mockTask}
        onClose={() => {}}
        onPause={vi.fn()}
        onResume={vi.fn()}
        onOpenFolder={vi.fn()}
        onCopyLink={vi.fn()}
        onRedownload={vi.fn()}
        onDelete={vi.fn()}
      />
    ));

    expect(container.querySelector('[role="menu"]')).toBeDefined();
    const items = container.querySelectorAll('[role="menuitem"]');
    expect(items.length).toBeGreaterThan(0);
  });

  it("打开时应自动聚焦第一个 menuitem", () =>
    new Promise<void>((resolve) => {
      const { container } = render(() => (
        <ContextMenu
          x={100}
          y={100}
          visible={true}
          task={mockTask}
          onClose={() => {}}
          onPause={vi.fn()}
          onResume={vi.fn()}
          onOpenFolder={vi.fn()}
          onCopyLink={vi.fn()}
          onRedownload={vi.fn()}
          onDelete={vi.fn()}
        />
      ));

      requestAnimationFrame(() => {
        const items =
          container.querySelectorAll<HTMLElement>('[role="menuitem"]');
        expect(document.activeElement).toBe(items[0]);
        resolve();
      });
    }));

  it("ArrowDown 应移动到下一项", () =>
    new Promise<void>((resolve) => {
      const { container } = render(() => (
        <ContextMenu
          x={100}
          y={100}
          visible={true}
          task={mockTask}
          onClose={() => {}}
          onPause={vi.fn()}
          onResume={vi.fn()}
          onOpenFolder={vi.fn()}
          onCopyLink={vi.fn()}
          onRedownload={vi.fn()}
          onDelete={vi.fn()}
        />
      ));

      requestAnimationFrame(() => {
        const items =
          container.querySelectorAll<HTMLElement>('[role="menuitem"]');
        fireEvent.keyDown(document, { key: "ArrowDown" });
        expect(document.activeElement).toBe(items[1]);
        resolve();
      });
    }));

  it("ArrowUp 应从第一项循环到最后一项", () =>
    new Promise<void>((resolve) => {
      const { container } = render(() => (
        <ContextMenu
          x={100}
          y={100}
          visible={true}
          task={mockTask}
          onClose={() => {}}
          onPause={vi.fn()}
          onResume={vi.fn()}
          onOpenFolder={vi.fn()}
          onCopyLink={vi.fn()}
          onRedownload={vi.fn()}
          onDelete={vi.fn()}
        />
      ));

      requestAnimationFrame(() => {
        const items =
          container.querySelectorAll<HTMLElement>('[role="menuitem"]');
        fireEvent.keyDown(document, { key: "ArrowUp" });
        expect(document.activeElement).toBe(items[items.length - 1]);
        resolve();
      });
    }));

  it("Escape 应触发 onClose", () =>
    new Promise<void>((resolve) => {
      const onClose = vi.fn();
      render(() => (
        <ContextMenu
          x={100}
          y={100}
          visible={true}
          task={mockTask}
          onClose={onClose}
          onPause={vi.fn()}
          onResume={vi.fn()}
          onOpenFolder={vi.fn()}
          onCopyLink={vi.fn()}
          onRedownload={vi.fn()}
          onDelete={vi.fn()}
        />
      ));

      requestAnimationFrame(() => {
        fireEvent.keyDown(document, { key: "Escape" });
        expect(onClose).toHaveBeenCalledTimes(1);
        resolve();
      });
    }));

  it("Enter 应触发当前聚焦项的 action", () =>
    new Promise<void>((resolve) => {
      const onPause = vi.fn();
      render(() => (
        <ContextMenu
          x={100}
          y={100}
          visible={true}
          task={mockTask}
          onClose={() => {}}
          onPause={onPause}
          onResume={vi.fn()}
          onOpenFolder={vi.fn()}
          onCopyLink={vi.fn()}
          onRedownload={vi.fn()}
          onDelete={vi.fn()}
        />
      ));

      requestAnimationFrame(() => {
        fireEvent.keyDown(document, { key: "Enter" });
        expect(onPause).toHaveBeenCalledWith("task-1");
        resolve();
      });
    }));

  it('已完成任务应显示"打开文件所在文件夹"', () => {
    const { container } = render(() => (
      <ContextMenu
        x={100}
        y={100}
        visible={true}
        task={completedTask}
        onClose={() => {}}
        onPause={vi.fn()}
        onResume={vi.fn()}
        onOpenFolder={vi.fn()}
        onCopyLink={vi.fn()}
        onRedownload={vi.fn()}
        onDelete={vi.fn()}
      />
    ));

    expect(container.textContent).toContain("打开文件所在文件夹");
  });

  it('已暂停任务应显示"恢复"而非"暂停"', () => {
    const { container } = render(() => (
      <ContextMenu
        x={100}
        y={100}
        visible={true}
        task={pausedTask}
        onClose={() => {}}
        onPause={vi.fn()}
        onResume={vi.fn()}
        onOpenFolder={vi.fn()}
        onCopyLink={vi.fn()}
        onRedownload={vi.fn()}
        onDelete={vi.fn()}
      />
    ));

    expect(container.textContent).toContain("恢复");
    expect(container.textContent).not.toContain("暂停");
  });
});
