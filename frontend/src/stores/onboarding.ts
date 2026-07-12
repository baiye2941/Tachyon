/**
 * 空列表 Onboarding 引导状态 store。
 *
 * 用于追踪用户是否已完成首次使用引导,决定是否高亮空列表的「新建任务」按钮。
 * localStorage key: tachyon.tasklist.onboarding.completed
 */

import { createRoot, createEffect, createSignal, type Accessor } from "solid-js";

const STORAGE_KEY = "tachyon.tasklist.onboarding.completed";

function load(): boolean {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === null) return false;
    const parsed = JSON.parse(raw) as unknown;
    return typeof parsed === "boolean" ? parsed : false;
  } catch {
    /* ignore */
  }
  return false;
}

function save(value: boolean): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(value));
  } catch {
    /* ignore */
  }
}

const [completed, setCompleted] = createSignal<boolean>(load());

/** 标记引导已完成(用户已熟悉界面,不再高亮) */
export function completeOnboarding(): void {
  setCompleted(true);
}

/** 重置引导状态(主要用于测试) */
export function resetOnboarding(): void {
  setCompleted(false);
}

export const $onboarding = {
  get isCompleted(): Accessor<boolean> {
    return completed;
  },
  completeOnboarding,
  resetOnboarding,
};

// 模块级 createEffect 监听状态变化并持久化。
// createRoot 提供 reactive owner,避免测试环境出现 warnings。
createRoot(() => {
  createEffect(() => {
    save(completed());
  });
});
