/**
 * 任务列表密度 store（comfortable / compact）。
 *
 * localStorage key: tachyon.tasklist.listDensity
 * 合法值: "comfortable" | "compact"
 */

import { createRoot, createEffect, createSignal, type Accessor } from "solid-js";
import type { ListDensity } from "../types";

const STORAGE_KEY = "tachyon.tasklist.listDensity";

const VALID_DENSITIES: ListDensity[] = ["comfortable", "compact"];

function getDefaultDensity(): ListDensity {
  return "comfortable";
}

function isValidDensity(value: unknown): value is ListDensity {
  return typeof value === "string" && (VALID_DENSITIES as string[]).includes(value);
}

function load(): ListDensity {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return getDefaultDensity();
    const parsed = JSON.parse(raw) as unknown;
    if (isValidDensity(parsed)) {
      return parsed;
    }
  } catch {
    /* ignore */
  }
  return getDefaultDensity();
}

function save(density: ListDensity): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(density));
  } catch {
    /* ignore */
  }
}

const [listDensity, setListDensityState] = createSignal<ListDensity>(load());

export function setListDensity(density: ListDensity): void {
  if (isValidDensity(density)) {
    setListDensityState(density);
  }
}

export function toggleListDensity(): void {
  setListDensityState((prev) =>
    prev === "comfortable" ? "compact" : "comfortable",
  );
}

export const $listDensity = {
  get density(): Accessor<ListDensity> {
    return listDensity;
  },
  setListDensity,
  toggleListDensity,
};

// 模块级 createEffect 监听状态变化并持久化。
// createRoot 提供 reactive owner，避免测试环境出现 warnings。
createRoot(() => {
  createEffect(() => {
    save(listDensity());
  });
});
