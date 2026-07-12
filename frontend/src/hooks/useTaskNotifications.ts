import { createEffect, onCleanup } from "solid-js";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { onTaskNotification } from "../api/events";
import { $config } from "../stores/settings";

/**
 * 任务终态系统通知 hook。
 *
 * - 当 notifications.enabled 为 true 时,检查并请求通知权限,然后监听
 *   `task-notification` 事件并调用原生通知 API。
 * - 当设置关闭时,停止监听并不再发送通知。
 * - 首次启动仅静默检查权限,不会主动弹窗打扰用户。
 */
export function useTaskNotifications() {
  let activeUnlisten: (() => void) | null = null;

  const stopListening = () => {
    if (activeUnlisten) {
      activeUnlisten();
      activeUnlisten = null;
    }
  };

  const ensureListening = async () => {
    if (activeUnlisten) return;

    let granted = await isPermissionGranted();
    if (!granted) {
      const permission = await requestPermission();
      granted = permission === "granted";
    }
    if (!granted) return;

    const unlisten = await onTaskNotification((payload) => {
      const enabled = $config.get()?.notifications?.enabled ?? true;
      if (!enabled) return;
      sendNotification({ title: payload.title, body: payload.body });
    });
    activeUnlisten = unlisten;
  };

  createEffect(() => {
    const cfg = $config.get();
    if (cfg === null) {
      // 等待应用配置加载完成,避免在已知用户偏好前请求权限
      return;
    }
    const enabled = cfg.notifications?.enabled ?? true;
    if (enabled) {
      ensureListening().catch(() => {
        // 权限或监听失败时静默降级,避免阻塞应用启动
      });
    } else {
      stopListening();
    }
  });

  onCleanup(stopListening);
}
