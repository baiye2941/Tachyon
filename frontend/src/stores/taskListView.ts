/**
 * 任务列表视图模式 store（按状态分组 / 平铺）。
 *
 * localStorage key: tachyon.tasklist.groupBy
 * 合法值: "none" | "status"
 */

import { createRoot, createEffect, createSignal, type Accessor } from "solid-js";

const STORAGE_KEY = "tachyon.tasklist.groupBy";

export type GroupByMode = "none" | "status";

const VALID_MODES: GroupByMode[] = ["none", "status"];

function getDefaultMode(): GroupByMode {
  return "none";
}

function load(): GroupByMode {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return getDefaultMode();
    const parsed = JSON.parse(raw) as unknown;
    if (
      typeof parsed === "string" &&
      (VALID_MODES as string[]).includes(parsed)
    ) {
      return parsed as GroupByMode;
    }
  } catch {
    /* ignore */
  }
  return getDefaultMode();
}

function save(mode: GroupByMode): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(mode));
  } catch {
    /* ignore */
  }
}

const [groupBy, setGroupByState] = createSignal<GroupByMode>(load());

export function setGroupBy(mode: GroupByMode): void {
  if ((VALID_MODES as string[]).includes(mode)) {
    setGroupByState(mode);
  }
}

export function toggleGroupBy(): void {
  setGroupByState((prev) => (prev === "none" ? "status" : "none"));
}

export const $taskListView = {
  get groupBy(): Accessor<GroupByMode> {
    return groupBy;
  },
  setGroupBy,
  toggleGroupBy,
};

// 模块级 createEffect 监听状态变化并持久化。
// createRoot 提供 reactive owner，避免测试环境出现 warnings。
createRoot(() => {
  createEffect(() => {
    save(groupBy());
  });
});
