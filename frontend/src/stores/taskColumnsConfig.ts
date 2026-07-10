/**
 * 任务列表列配置 store（持久化到 localStorage）。
 */

import { createRoot, createEffect } from "solid-js";
import { createStore, produce } from "solid-js/store";
import {
  ALL_COLUMNS,
  DEFAULT_VISIBLE_KEYS,
  type ColumnKey,
  type ColumnDef,
} from "../components/taskColumns";

const STORAGE_KEY = "tachyon.tasklist.columns";
const VERSION = 1;

interface PersistedState {
  version: number;
  visible: ColumnKey[];
  widths: Partial<Record<ColumnKey, number>>;
}

const VALID_KEYS = new Set(ALL_COLUMNS.map((c) => c.key));

function getDefaultState(): PersistedState {
  return {
    version: VERSION,
    visible: [...DEFAULT_VISIBLE_KEYS],
    widths: {},
  };
}

function load(): PersistedState {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return getDefaultState();

    const parsed = JSON.parse(raw) as Partial<PersistedState>;
    if (parsed.version !== VERSION) return getDefaultState();

    const visible = Array.isArray(parsed.visible)
      ? parsed.visible.filter((k): k is ColumnKey =>
          VALID_KEYS.has(k as ColumnKey),
        )
      : [...DEFAULT_VISIBLE_KEYS];

    // name 列强制可见
    if (!visible.includes("name")) {
      visible.unshift("name");
    }

    const widths: PersistedState["widths"] = {};
    if (parsed.widths && typeof parsed.widths === "object") {
      for (const [key, value] of Object.entries(parsed.widths)) {
        if (
          VALID_KEYS.has(key as ColumnKey) &&
          typeof value === "number" &&
          value > 0
        ) {
          widths[key as ColumnKey] = value;
        }
      }
    }

    return { version: VERSION, visible, widths };
  } catch {
    return getDefaultState();
  }
}

function save(state: PersistedState): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    /* ignore */
  }
}

const [state, setState] = createStore<PersistedState>(load());

function visibleKeys(): ColumnKey[] {
  return state.visible;
}

function visibleColumns(): ColumnDef[] {
  return state.visible
    .map((key) => ALL_COLUMNS.find((c) => c.key === key))
    .filter((c): c is ColumnDef => c !== undefined);
}

function width(key: ColumnKey): number | "flex-1" {
  const def = ALL_COLUMNS.find((c) => c.key === key);
  if (!def) return "flex-1";
  return state.widths[key] ?? def.defaultWidth;
}

function widths(): Partial<Record<ColumnKey, number>> {
  return { ...state.widths };
}

function toggleVisibility(key: ColumnKey): void {
  if (key === "name") return;
  setState(
    produce((s) => {
      if (s.visible.includes(key)) {
        s.visible = s.visible.filter((k) => k !== key);
      } else {
        s.visible.push(key);
      }
    }),
  );
}

function setWidth(key: ColumnKey, value: number | "flex-1"): void {
  if (typeof value === "number") {
    const def = ALL_COLUMNS.find((c) => c.key === key);
    const clamped = Math.max(def?.minWidth ?? 0, value);
    setState(
      produce((s) => {
        s.widths[key] = clamped;
      }),
    );
  } else if (value === "flex-1") {
    setState(
      produce((s) => {
        delete s.widths[key];
      }),
    );
  }
}

function resetColumns(): void {
  setState(getDefaultState());
}

// 模块级 createEffect 监听状态变化并持久化。
// createRoot 提供 reactive owner，避免测试环境出现 warnings。
createRoot(() => {
  createEffect(() => {
    save({
      version: state.version,
      visible: [...state.visible],
      widths: { ...state.widths },
    });
  });
});

export const $taskColumns = {
  state,
  visibleKeys,
  visibleColumns,
  width,
  widths,
  toggleVisibility,
  setWidth,
  resetColumns,
};
