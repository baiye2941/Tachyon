/**
 * 异步请求身份：单调 seq + 快照比对（审计 FT-10）。
 * 纯函数，便于 vitest 验证陈旧响应丢弃规则。
 */

export function nextRequestSeq(current: number): number {
  return current + 1
}

/** 响应是否仍对应当前 seq */
export function isCurrentSeq(responseSeq: number, currentSeq: number): boolean {
  return responseSeq === currentSeq
}

/** probe 结果是否仍匹配当前 URL */
export function shouldApplyProbeResult(
  responseSeq: number,
  currentSeq: number,
  requestUrl: string,
  currentUrl: string | undefined,
): boolean {
  return isCurrentSeq(responseSeq, currentSeq) && currentUrl === requestUrl
}

/** HF 预览是否仍匹配当前 repo/revision */
export function shouldApplyHfPreview(
  responseSeq: number,
  currentSeq: number,
  request: { repoId: string; revision?: string },
  current: { repoId: string; revision?: string } | null,
): boolean {
  if (!isCurrentSeq(responseSeq, currentSeq) || !current) return false
  return (
    current.repoId === request.repoId &&
    (current.revision ?? undefined) === (request.revision ?? undefined)
  )
}

/**
 * 嗅探配置乐观更新：失败时应用 previous（审计 FT-11）。
 */
export function resolveOptimisticConfigOnFailure<T>(
  previous: T | null,
  _attempted: T,
  failed: boolean,
): T | null {
  if (failed) return previous
  return _attempted
}
