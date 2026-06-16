import { createSignal } from "solid-js";
import type { TaskInfo } from "../types";
import { $tasks } from "../stores/downloads";

/**
 * 任务右键菜单状态管理。
 */
export function useContextMenu() {
  const [contextMenu, setContextMenu] = createSignal<{
    visible: boolean;
    x: number;
    y: number;
    task: TaskInfo | null;
  }>({ visible: false, x: 0, y: 0, task: null });

  const open = (e: MouseEvent, taskId: string) => {
    e.preventDefault();
    const task = $tasks.get().find((t) => t.id === taskId) || null;
    setContextMenu({ visible: true, x: e.clientX, y: e.clientY, task });
  };

  const close = () =>
    setContextMenu((prev) => ({ ...prev, visible: false }));

  return { contextMenu, open, close };
}
