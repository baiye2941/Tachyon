import { errorMessage } from "../utils/appError";
import { onMount, onCleanup } from "solid-js";
import { api } from "../api/invoke";
import { onClipboardUrlDetected, onRecoveryWarning, onSnifferResourceAdded } from "../api/events";
import {
  $activeCount,
  $totalSpeed,
  refreshTaskList,
} from "../stores/downloads";
import { addToast } from "../stores/toast";
import { addToast as addRichToast } from "../components/ToastContainer";
import * as speedHistory from "../stores/speedHistory";
import { isBrowserDev, startMockData } from "../stores/mockData";
import type { SnifferResource } from "../types";
import { tr } from "../i18n";
import { useProgressListener } from "./useTauriEvent";
import { useRafThrottle } from "./useRafThrottle";
import { loadShortcuts } from "../stores/shortcuts";

/**
 * 应用级初始化：任务列表刷新、进度订阅、恢复告警、嗅探资源加载与实时推送、
 * 剪贴板 URL 检测提示、500ms 节流的速度历史记录、以及快捷键覆盖配置加载。
 *
 * 浏览器 dev 环境(无 Tauri 后端)激活 mock 数据源,让 UI 效果可被查看;
 * Tauri 生产环境走真实 api。判断见 isBrowserDev()。
 */
export function useAppInit(
  setSnifferResources: (resources: SnifferResource[]) => void,
  addSnifferResource: (resource: SnifferResource) => void,
) {
  // 浏览器 dev:注入 mock 种子任务 + 模拟进度 tick,跳过真实 api
  if (isBrowserDev()) {
    startMockData();
  }

  // 真实数据订阅(Tauri 环境生效)
  useProgressListener();

  onMount(() => {
    // 先加载快捷键覆盖配置,确保全局键盘监听使用最新绑定
    loadShortcuts();

    if (isBrowserDev()) {
      // 浏览器 dev:mock 已注入,无需真实订阅
      return;
    }

    refreshTaskList();

    api
      .subscribeProgress()
      .catch((e) => addToast(tr("toast.progressSubscribeFailed", { error: errorMessage(e) }), "error"));

    // 监听启动恢复告警(损坏的断点续传快照已被跳过)
    const recoveryUnlistenPromise = onRecoveryWarning((payload) => {
      if (payload && payload.count > 0) {
        // 兼容层 addToast 仅支持 info/success/error 三态,warning 语义映射为 info
        addToast(
          tr("toast.recoveryWarning", { count: payload.count }),
          "info",
        );
      }
    }).catch((e) => {
      addToast(tr("toast.recoveryListenFailed", { error: errorMessage(e) }), "error");
      // 监听失败时返回 no-op unlisten,保证下游 then(fn => fn()) 安全
      return (() => {}) as (() => void);
    });

    // 监听新嗅探资源事件,实时追加到列表
    const snifferUnlistenPromise = onSnifferResourceAdded((resource) => {
      addSnifferResource(resource);
    }).catch((e) => {
      addToast(tr("toast.snifferListenFailed", { error: errorMessage(e) }), "error");
      return (() => {}) as (() => void);
    });

    // 监听剪贴板 URL 检测事件,弹富结构 Toast 让用户确认下载
    const clipboardUnlistenPromise = onClipboardUrlDetected((payload) => {
      addRichToast({
        type: "info",
        title: tr("toast.clipboardUrlDetected"),
        description: payload.url.length > 80
          ? payload.url.slice(0, 77) + "..."
          : payload.url,
        actions: [
          {
            label: tr("toast.clipboardUrlDownload"),
            onClick: () => {
              api
                .createTask(payload.url)
                .then(() => refreshTaskList())
                .catch((e) =>
                  addToast(
                    tr("toast.clipboardUrlDownloadFailed", { error: errorMessage(e) }),
                    "error",
                  ),
                );
            },
          },
          {
            label: tr("toast.clipboardUrlIgnore"),
            onClick: () => {},
          },
        ],
        duration: 8000,
      });
    }).catch((e) => {
      addToast(tr("toast.snifferListenFailed", { error: errorMessage(e) }), "error");
      return (() => {}) as (() => void);
    });

    onCleanup(() => {
      recoveryUnlistenPromise.then((fn) => fn());
      snifferUnlistenPromise.then((fn) => fn());
      clipboardUnlistenPromise.then((fn) => fn());
    });

    // 加载 sniffer 资源
    api
      .getSnifferResources()
      .then(setSnifferResources)
      .catch((e) => addToast(tr("toast.snifferLoadFailed", { error: errorMessage(e) }), "error"));
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
