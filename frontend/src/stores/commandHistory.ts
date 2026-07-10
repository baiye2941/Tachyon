import { createStore } from "solid-js/store";
import { COMMANDS } from "../commands/registry";

const RECENT_KEY = "tachyon.cmd.recent";
const PINNED_KEY = "tachyon.cmd.pinned";
const MAX_RECENT = 8;

function loadIds(key: string): string[] {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    const validIds = new Set(COMMANDS.map((c) => c.id));
    return parsed.filter(
      (id): id is string => typeof id === "string" && validIds.has(id),
    );
  } catch {
    return [];
  }
}

function saveIds(key: string, ids: string[]): void {
  try {
    localStorage.setItem(key, JSON.stringify(ids));
  } catch {
    /* ignore */
  }
}

const [$recent, setRecent] = createStore<string[]>(loadIds(RECENT_KEY));
const [$pinned, setPinned] = createStore<string[]>(loadIds(PINNED_KEY));

export { $recent, $pinned };

/** 将命令加入最近使用(去重、置顶最前、最多 8 条) */
export function addRecentCommand(id: string): void {
  if (!COMMANDS.some((c) => c.id === id)) return;
  setRecent((prev) => {
    const next = [id, ...prev.filter((x) => x !== id)];
    const trimmed = next.slice(0, MAX_RECENT);
    saveIds(RECENT_KEY, trimmed);
    return trimmed;
  });
}

/** 切换命令置顶状态 */
export function togglePinnedCommand(id: string): void {
  if (!COMMANDS.some((c) => c.id === id)) return;
  setPinned((prev) => {
    const idx = prev.indexOf(id);
    const next =
      idx === -1
        ? [...prev, id]
        : [...prev.slice(0, idx), ...prev.slice(idx + 1)];
    saveIds(PINNED_KEY, next);
    return next;
  });
}

/** 判断命令是否已置顶 */
export function isPinned(id: string): boolean {
  return $pinned.includes(id);
}
