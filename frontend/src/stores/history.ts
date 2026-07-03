import { createStore } from "solid-js/store";
import { untrack } from "solid-js";
import { tr } from "../i18n";

export type HistoryFilter = "all" | "completed" | "failed" | "cancelled";

export interface HistoryRecord {
  id: string;
  url: string;
  fileName: string;
  fileSize: number;
  status: "completed" | "failed" | "cancelled";
  duration: number;
  avgSpeed: number;
  completedAt: string;
  /**
   * 文件保存路径,用于"打开所在文件夹"功能。
   * 旧版 localStorage 记录可能没有此字段(默认空字符串),允许兼容。
   */
  savePath: string;
}

export interface HistoryStats {
  totalDownloads: number;
  totalBytes: number;
  avgSpeed: number;
  successRate: number;
  totalDuration: number;
  completedCount: number;
  failedCount: number;
  cancelledCount: number;
  maxSpeed: number;
  maxFile: HistoryRecord | null;
}

const STORAGE_KEY = "tachyon:download_history";
const MAX_RECORDS = 100;

/**
 * 脱敏 URL 中的敏感查询参数(token, signature, key, secret 等)
 * 防止 localStorage 中残留认证凭据
 */
const SENSITIVE_PARAMS = [
  "token",
  "access_token",
  "auth",
  "signature",
  "sign",
  "key",
  "secret",
  "password",
  "pass",
  "credential",
  "session_id",
  "sessionid",
  "api_key",
  "apikey",
  "private_token",
];

function redactUrl(url: string): string {
  try {
    const parsed = new URL(url);
    const paramsToDelete: string[] = [];
    parsed.searchParams.forEach((_value, key) => {
      if (SENSITIVE_PARAMS.includes(key.toLowerCase())) {
        paramsToDelete.push(key);
      }
    });
    paramsToDelete.forEach((key) => parsed.searchParams.set(key, "[REDACTED]"));
    return parsed.toString();
  } catch {
    // URL 解析失败,返回原始值(不阻断业务)
    return url;
  }
}

function generateId(): string {
  return `${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;
}

function isValidRecord(r: unknown): r is HistoryRecord {
  if (typeof r !== "object" || r === null) return false;
  const rec = r as Record<string, unknown>;
  return (
    typeof rec.id === "string" &&
    typeof rec.url === "string" &&
    typeof rec.fileName === "string" &&
    (rec.status === "completed" ||
      rec.status === "failed" ||
      rec.status === "cancelled")
  );
}

function loadFromStorage(): HistoryRecord[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    // 旧版记录可能没有 savePath 字段,补默认空字符串保持向后兼容
    return parsed.filter(isValidRecord).map((r) => ({
      ...r,
      savePath: typeof r.savePath === "string" ? r.savePath : "",
    }));
  } catch {
    return [];
  }
}

function saveToStorage(records: HistoryRecord[]) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(records));
  } catch (e) {
    console.warn(tr("toast.historySaveFailed"), e);
  }
}

const [historyRecords, setHistoryRecords] =
  createStore<HistoryRecord[]>(loadFromStorage());

export { historyRecords };

export function addHistoryRecord(
  record: Omit<HistoryRecord, "id" | "completedAt" | "savePath"> & {
    /** 保存路径,用于"打开所在文件夹"。可选,缺省为空字符串 */
    savePath?: string;
  },
): void {
  // 脱敏 URL 中的敏感参数(token/signature/key 等),防止 localStorage 泄漏
  const sanitizedRecord = {
    ...record,
    url: redactUrl(record.url),
    savePath: record.savePath ?? "",
  };
  const newRecord: HistoryRecord = {
    ...sanitizedRecord,
    id: generateId(),
    completedAt: new Date().toISOString(),
  };
  setHistoryRecords((prev) => {
    const updated = [newRecord, ...prev];
    if (updated.length > MAX_RECORDS) {
      const trimmed = updated.slice(0, MAX_RECORDS);
      saveToStorage(trimmed);
      return trimmed;
    }
    saveToStorage(updated);
    return updated;
  });
}

export function getHistoryRecords(
  filter: HistoryFilter = "all",
): HistoryRecord[] {
  const records = untrack(() => historyRecords);
  if (filter === "all") return [...records];
  return records.filter((r) => r.status === filter);
}

// 单次遍历统计，替代原来 3 次 reduce + 3 次 filter
export function getHistoryStats(): HistoryStats {
  return getHistoryStatsForRecords(untrack(() => historyRecords));
}

export function getHistoryStatsForRecords(
  records: HistoryRecord[],
): HistoryStats {
  let totalBytes = 0;
  let totalDuration = 0;
  let speedSum = 0;
  let completedCount = 0;
  let failedCount = 0;
  let cancelledCount = 0;
  let maxSpeed = 0;
  let maxFile: HistoryRecord | null = null;

  for (let i = 0; i < records.length; i++) {
    const r = records[i]!;
    totalBytes += r.fileSize || 0;
    totalDuration += r.duration || 0;
    speedSum += r.avgSpeed || 0;
    if ((r.avgSpeed || 0) > maxSpeed) maxSpeed = r.avgSpeed || 0;
    if (!maxFile || (r.fileSize || 0) > (maxFile.fileSize || 0)) maxFile = r;
    if (r.status === "completed") completedCount++;
    else if (r.status === "failed") failedCount++;
    else if (r.status === "cancelled") cancelledCount++;
  }

  const totalDownloads = records.length;
  const avgSpeed = totalDownloads > 0 ? speedSum / totalDownloads : 0;
  const successRate = totalDownloads > 0 ? completedCount / totalDownloads : 0;

  return {
    totalDownloads,
    totalBytes,
    avgSpeed,
    successRate,
    totalDuration,
    completedCount,
    failedCount,
    cancelledCount,
    maxSpeed,
    maxFile,
  };
}

export function clearHistory(): void {
  setHistoryRecords([]);
  saveToStorage([]);
}

export function getRecordById(id: string): HistoryRecord | undefined {
  const records = untrack(() => historyRecords);
  return records.find((r) => r.id === id);
}

export function deleteHistoryRecord(id: string): void {
  setHistoryRecords((prev) => {
    const idx = prev.findIndex((r) => r.id === id);
    if (idx === -1) return prev;
    const updated = [...prev.slice(0, idx), ...prev.slice(idx + 1)];
    saveToStorage(updated);
    return updated;
  });
}
