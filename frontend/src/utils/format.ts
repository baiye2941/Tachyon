import {
  FileIcon,
  VideoIcon,
  AudioIcon,
  DocumentIcon,
  ImageIcon,
  ArchiveIcon,
  GearIcon,
  AttachmentIcon,
} from "../components/icons";
import { tr, type MessageKey } from "../i18n";

export function formatSize(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined) return tr("common.unknown");
  if (bytes === 0) return "0 B";
  if (bytes >= 1024 * 1024 * 1024 * 1024)
    return `${(bytes / 1024 / 1024 / 1024 / 1024).toFixed(1)} TB`;
  if (bytes >= 1024 * 1024 * 1024)
    return `${(bytes / 1024 / 1024 / 1024).toFixed(1)} GB`;
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${bytes} B`;
}

export function formatSpeed(bytesPerSec: number): string {
  if (bytesPerSec === 0) return "---";
  if (bytesPerSec >= 1024 * 1024 * 1024)
    return `${(bytesPerSec / 1024 / 1024 / 1024).toFixed(1)} GB/s`;
  if (bytesPerSec >= 1024 * 1024)
    return `${(bytesPerSec / 1024 / 1024).toFixed(1)} MB/s`;
  if (bytesPerSec >= 1024) return `${(bytesPerSec / 1024).toFixed(1)} KB/s`;
  return `${bytesPerSec} B/s`;
}

/**
 * 文件类型 → 图标 + 颜色映射。
 *
 * color 返回 CSS 变量字符串(var(--color-file-*)),供 DOM 内联样式使用。
 * 与 index.css @theme 的 --color-file-* token 同源。
 * 颜色降饱和度,各类型用独立 hue 区分(video 橙/audio 紫/document 蓝/image 绿/archive 橙/model 紫)。
 */
const FILE_TYPE_MAP: Record<string, { icon: typeof FileIcon; color: string }> =
  {
    video: { icon: VideoIcon, color: "var(--color-file-video)" },
    audio: { icon: AudioIcon, color: "var(--color-file-audio)" },
    document: { icon: DocumentIcon, color: "var(--color-file-document)" },
    image: { icon: ImageIcon, color: "var(--color-file-image)" },
    archive: { icon: ArchiveIcon, color: "var(--color-file-archive)" },
    executable: { icon: GearIcon, color: "var(--color-file-executable)" },
    model: { icon: GearIcon, color: "var(--color-file-model)" },
  };

export const EXT_TYPE_MAP: Record<string, string> = {
  mp4: "video",
  mkv: "video",
  avi: "video",
  mov: "video",
  webm: "video",
  mp3: "audio",
  wav: "audio",
  flac: "audio",
  aac: "audio",
  ogg: "audio",
  pdf: "document",
  doc: "document",
  docx: "document",
  txt: "document",
  xls: "document",
  xlsx: "document",
  jpg: "image",
  jpeg: "image",
  png: "image",
  gif: "image",
  webp: "image",
  svg: "image",
  zip: "archive",
  rar: "archive",
  "7z": "archive",
  tar: "archive",
  gz: "archive",
  exe: "executable",
  msi: "executable",
  dmg: "executable",
  sh: "executable",
  safetensors: "model",
  gguf: "model",
  pt: "model",
  pth: "model",
  onnx: "model",
  bin: "model",
};

export function getFileType(fileName: string): {
  icon: typeof FileIcon;
  color: string;
} {
  const ext = fileName.split(".").pop()?.toLowerCase() || "";
  const type = EXT_TYPE_MAP[ext];
  const entry = type ? FILE_TYPE_MAP[type] : undefined;
  return entry
    ? { icon: entry.icon, color: entry.color }
    : { icon: AttachmentIcon, color: "var(--color-file-other)" };
}

export function getFileTypeColor(type: string): string {
  return FILE_TYPE_MAP[type]?.color ?? "var(--color-file-other)";
}

/**
 * 返回状态语义色(DOM 用,值为 CSS 变量字符串)。
 *
 * 状态色语义:
 * - 下载中 = 冷电青(品牌色绑定核心动态状态)
 * - 完成   = 青绿 emerald(与下载中电青对立,色盲可区分)
 * - 暂停   = ash(中性灰,警示但不紧急)
 * - 连接/校验/恢复 = cyan/电青 过渡态
 */
export function getStatusColor(status: string): string {
  switch (status) {
    case "downloading":
      return "var(--color-status-downloading)";
    case "pending":
      return "var(--color-status-pending)";
    case "paused":
      return "var(--color-status-paused)";
    case "completed":
      return "var(--color-status-completed)";
    case "failed":
      return "var(--color-status-error)";
    case "connecting":
      return "var(--color-status-connecting)";
    case "verifying":
      return "var(--color-status-verifying)";
    case "resuming":
      return "var(--color-status-resuming)";
    default:
      return "var(--color-status-pending)";
  }
}

export function getStatusLabel(status: string): string {
  const map: Record<string, MessageKey> = {
    downloading: "status.label.downloading",
    pending: "status.label.pending",
    paused: "status.label.paused",
    completed: "status.label.completed",
    failed: "status.label.failed",
    connecting: "status.label.connecting",
    verifying: "status.label.verifying",
    resuming: "status.label.resuming",
  };
  const key = map[status];
  return key ? tr(key) : status;
}

export function formatETA(speed: number, remaining: number): string {
  if (speed <= 0 || remaining <= 0) return "---";
  const seconds = Math.ceil(remaining / speed);
  if (seconds < 60) return tr("time.seconds", { n: seconds });
  if (seconds < 3600) {
    const m = Math.floor(seconds / 60);
    const s = seconds % 60;
    return tr("time.minutesSeconds", { m, s });
  }
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  return tr("time.hoursMinutes", { h, m });
}

export function formatDate(iso: string): string {
  const d = new Date(iso);
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
