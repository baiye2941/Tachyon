/**
 * Dev-only mock 数据源(仅非 Tauri 环境激活)。
 *
 * 用途:`bun run dev` 在浏览器(无 Tauri 后端)时注入参考稿风格的
 * 种子任务 + 模拟进度 tick,让 UI 视觉效果可被实际查看。
 *
 * 激活条件:`!('__TAURI_INTERNALS__' in window)` —— 生产 Tauri 构建里
 * 该判断恒为 false,本模块所有注入与定时器都不会启动,零生产影响。
 *
 * 数据走真实 `setTasks` + `updateProgress`,下游 hot 层 / speedHistory /
 * 状态机 / 详情面板 / chunk 网格全部按真实路径激活。
 */
import { onCleanup } from "solid-js";
import type { TaskInfo, ProgressPayload } from "../types";
import { $tasks, updateProgress } from "../stores/downloads";

/** 判断当前是否为浏览器 dev 环境(无 Tauri 后端,且非测试环境)。
 *  测试环境(Vitest)不激活,避免污染 useAppInit.spec 等断言真实 api 调用。 */
export function isBrowserDev(): boolean {
  if (typeof window === "undefined") return false;
  if ("__TAURI_INTERNALS__" in window || "__TAURI__" in window) return false;
  // Vitest 注入 MODE=test;仅 dev 模式才 mock
  if (import.meta.env.MODE === "test") return false;
  return import.meta.env.DEV;
}

export function removeMockTask(taskId: string): boolean {
  if (!isBrowserDev()) return false;
  const next = $tasks.get().filter((task) => task.id !== taskId);
  if (next.length === $tasks.get().length) return false;
  $tasks.set(next);
  return true;
}

const now = Date.now();

function makeTask(
  id: string,
  name: string,
  ext: string,
  sizeMB: number,
  progress: number,
  status: TaskInfo["status"],
  speed: number,
  url: string,
  createdAtOffset: number,
): TaskInfo {
  const size = Math.round(sizeMB * 1024 * 1024);
  const fragmentsTotal = Math.min(64, Math.max(12, Math.round(Math.cbrt(sizeMB) * 3)));
  return {
    id,
    url,
    fileName: `${name}.${ext}`,
    fileSize: size,
    downloaded: Math.round(size * progress),
    speed,
    status,
    progress,
    fragmentsTotal,
    fragmentsDone: Math.floor(fragmentsTotal * progress),
    createdAt: new Date(now - createdAtOffset * 1000).toISOString(),
    savePath: `~/Downloads/${name}.${ext}`,
  };
}

/** 参考稿风格种子任务(覆盖各状态/文件类型) */
function seedTasks(): TaskInfo[] {
  return [
    makeTask("t1", "ubuntu-24.04.1-desktop-amd64", "iso", 5400, 0.62, "downloading", 12.4e6, "https://cdn.tachyon.dev/pkg/ubuntu-24.04.1.iso", 8),
    makeTask("t2", "Sintel.2010.2160p.HDR.BT2020", "mkv", 14800, 0.34, "downloading", 28.1e6, "https://cdn.tachyon.dev/pkg/Sintel.2010.mkv", 16),
    makeTask("t3", "node-v22.9.0-linux-x64", "tar.xz", 42, 1, "completed", 0, "https://nodejs.org/dist/v22.9.0/node-v22.9.0-linux-x64.tar.xz", 90),
    makeTask("t4", "dataset-imagenet-mini", "zip", 2300, 0.78, "downloading", 9.7e6, "https://cdn.tachyon.dev/pkg/imagenet-mini.zip", 24),
    makeTask("t5", "macos-sonoma-installer", "dmg", 12200, 0.12, "paused", 0, "https://cdn.tachyon.dev/pkg/macos-sonoma.dmg", 40),
    makeTask("t6", "react-conf-2025-keynote", "mp4", 1800, 0.45, "failed", 0, "https://cdn.tachyon.dev/pkg/react-conf-2025.mp4", 52),
    makeTask("t7", "postgresql-16.4-docs", "pdf", 38, 1, "completed", 0, "https://www.postgresql.org/files/documentation/pdf/16.4/postgresql-16.4-US.pdf", 120),
    makeTask("t8", "rustup-init-x86_64", "exe", 12, 0, "pending", 0, "https://win.rustup.rs/x86_64", 2),
    makeTask("t9", "lofi-beats-collection-vol3", "flac", 640, 0.88, "downloading", 4.2e6, "https://cdn.tachyon.dev/pkg/lofi-vol3.flac", 30),
    makeTask("t10", "figma-export-assets-batch", "zip", 184, 0.55, "downloading", 6.6e6, "https://cdn.tachyon.dev/pkg/figma-batch.zip", 12),
    makeTask("t11", "kubernetes-v1.31.0-src", "tar.gz", 96, 1, "completed", 0, "https://github.com/kubernetes/kubernetes/archive/refs/tags/v1.31.0.tar.gz", 200),
    makeTask("t12", "wallpaper-pack-8k-nature", "png", 920, 0.23, "paused", 0, "https://cdn.tachyon.dev/pkg/wallpaper-8k.png", 60),
  ];
}

/**
 * 启动 mock:注入种子任务 + 1s tick 模拟进度。
 * 返回 cleanup(由 onCleanup 调用)。
 */
export function startMockData(): void {
  if (!isBrowserDev()) return;

  // 注入种子任务
  $tasks.set(seedTasks());

  // 1s tick:downloading 任务推进进度 + 速度抖动
  const iv = setInterval(() => {
    const tasks = $tasks.get();
    const payload: Record<string, ProgressPayload> = {};
    let changed = false;
    for (const t of tasks) {
      if (t.status !== "downloading") continue;
      const inc = t.speed * 1.05; // ~1s 进度
      const received = Math.min(t.fileSize || 0, (t.downloaded || 0) + inc);
      const size = t.fileSize || 0;
      const pct = size > 0 ? received / size : 0;
      const completed = received >= size;
      // 速度抖动 ±6%
      const jitter = 1 + (Math.random() - 0.5) * 0.12;
      payload[t.id] = {
        id: t.id,
        progress: pct,
        downloaded: received,
        speed: completed ? 0 : Math.max(0.5e6, t.speed * jitter),
        status: completed ? "completed" : "downloading",
        fragmentsDone: Math.floor((t.fragmentsTotal || 0) * pct),
        fragmentsTotal: 0,
        activeConcurrency: 0,
      };
      changed = true;
    }
    if (changed) updateProgress(payload);
  }, 1000);

  onCleanup(() => clearInterval(iv));
}
