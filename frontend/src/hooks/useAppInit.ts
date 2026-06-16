import { onMount } from "solid-js";
import { api } from "../api/invoke";
import { onRecoveryWarning } from "../api/events";
import {
  $activeCount,
  $totalSpeed,
  refreshTaskList,
} from "../stores/downloads";
import { addToast } from "../stores/toast";
import * as speedHistory from "../stores/speedHistory";
import type { SnifferResource } from "../types";
import { useProgressListener } from "./useTauriEvent";
import { useRafThrottle } from "./useRafThrottle";

/**
 * 应用级初始化：任务列表刷新、进度订阅、恢复告警、嗅探资源加载、
 * 以及 500ms 节流的速度历史记录。
 */
export function useAppInit(
  setSnifferResources: (resources: SnifferResource[]) => void,
) {
  // 真实数据订阅
  useProgressListener();

  onMount(() => {
    refreshTaskList();

    api
      .subscribeProgress()
      .catch((e) => addToast(`进度订阅失败: ${e}`, "error"));

    // 监听启动恢复告警(损坏的断点续传快照已被跳过)
    onRecoveryWarning((payload) => {
      if (payload && payload.count > 0) {
        // 兼容层 addToast 仅支持 info/success/error 三态,warning 语义映射为 info
        addToast(
          `检测到 ${payload.count} 个损坏的断点续传记录，已跳过恢复`,
          "info",
        );
      }
    }).catch((e) => addToast(`恢复告警监听失败: ${e}`, "error"));

    // 加载 sniffer 资源
    api
      .getSnifferResources()
      .then(setSnifferResources)
      .catch((e) => addToast(`嗅探资源加载失败: ${e}`, "error"));
  });

  // speedHistory effect:500ms 时间节流 + rAF 批量,避免 reactive storm
  let lastSpeedHistoryUpdate = 0;
  useRafThrottle({
    source: () => ({ speed: $totalSpeed.get(), count: $activeCount.get() }),
    callback: ({ speed, count }) => {
      const now = Date.now();
      if (now - lastSpeedHistoryUpdate < 500) return;
      lastSpeedHistoryUpdate = now;
      speedHistory.pushSpeed(speed);
      speedHistory.setActiveTasksCount(count);
    },
  });
}
