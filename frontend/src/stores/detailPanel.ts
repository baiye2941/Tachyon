/**
 * 详情面板侧栏宽度 store。
 *
 * localStorage key: tachyon.detailPanel.width
 * 合法范围: [280, 600]
 */

import { createRoot, createEffect, createSignal, type Accessor } from "solid-js";

export const STORAGE_KEY = "tachyon.detailPanel.width";
export const DEFAULT_WIDTH = 360;
export const MIN_WIDTH = 280;
export const MAX_WIDTH = 600;

function clampWidth(value: number): number {
  return Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, Math.round(value)));
}

function isValidWidth(value: unknown): value is number {
  return (
    typeof value === "number" &&
    Number.isFinite(value) &&
    value >= MIN_WIDTH &&
    value <= MAX_WIDTH
  );
}

function load(): number {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return DEFAULT_WIDTH;
    const parsed = JSON.parse(raw) as unknown;
    if (isValidWidth(parsed)) {
      return Math.round(parsed);
    }
  } catch {
    /* ignore */
  }
  return DEFAULT_WIDTH;
}

function save(width: number): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(width));
  } catch {
    /* ignore */
  }
}

const [panelWidth, setPanelWidthState] = createSignal<number>(load());

export function setWidth(value: number): void {
  if (!Number.isFinite(value)) return;
  setPanelWidthState(clampWidth(value));
}

export function resetWidth(): void {
  setPanelWidthState(DEFAULT_WIDTH);
}

export const $detailPanel = {
  get width(): Accessor<number> {
    return panelWidth;
  },
  setWidth,
  resetWidth,
};

// 模块级 createEffect 监听状态变化并持久化。
// createRoot 提供 reactive owner，避免测试环境出现 warnings。
createRoot(() => {
  createEffect(() => {
    save(panelWidth());
  });
});
