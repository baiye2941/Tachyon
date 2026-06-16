import { createSignal } from "solid-js";
import { api } from "../api/invoke";
import { parseDroppedFiles } from "../utils/dragDrop";
import { refreshTaskList } from "../stores/downloads";
import { addToast } from "../stores/toast";

/**
 * 全局拖放创建下载任务。
 *
 * 优先处理拖入的 .txt 文件(逐行解析 URL),回退到纯文本链接。
 */
export function useDragDrop() {
  const [isDragOver, setIsDragOver] = createSignal(false);

  const handleDragOver = (e: DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setIsDragOver(true);
  };

  const handleDragLeave = (e: DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const x = e.clientX;
    const y = e.clientY;
    if (
      x <= rect.left ||
      x >= rect.right ||
      y <= rect.top ||
      y >= rect.bottom
    ) {
      setIsDragOver(false);
    }
  };

  const createTasks = (urls: string[]) => {
    urls.forEach((url) => {
      api
        .createTask(url.trim())
        .catch((e) => addToast(`创建任务失败: ${e}`, "error"));
    });
    setTimeout(() => refreshTaskList(), 300);
  };

  const handleDrop = async (e: DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setIsDragOver(false);

    // 优先处理拖入的文件(.txt 逐行解析)
    const fileUrls = await parseDroppedFiles(e.dataTransfer?.files);
    if (fileUrls.length > 0) {
      createTasks(fileUrls);
      return;
    }

    // 回退到文本链接
    const text = e.dataTransfer?.getData("text/plain");
    if (text) {
      createTasks(text.split("\n").filter((u) => u.trim()));
    }
  };

  return {
    isDragOver,
    handleDragOver,
    handleDragLeave,
    handleDrop,
  };
}
