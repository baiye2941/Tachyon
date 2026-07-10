/** 检测 tracker URL 基本格式 */
export function isValidTrackerUrl(url: string): boolean {
  const trimmed = url.trim();
  if (!trimmed) return false;
  return /^udp:\/\/.+|^https?:\/\/.+/.test(trimmed);
}
