// 错误归一化层(P2-10)
//
// 后端 AppError 序列化为结构化对象:
//   { type: "Network"|"Config"|"Core"|..., message: string, inner?: DownloadError }
// 其中 Core 变体嵌套 DownloadError:
//   { type: "Network"|"Protocol"|"Throttled"|"Forbidden"|...,
//     message: string, retryable: boolean,
//     retryAfterSecs?: number, status?: number, reason?: string,
//     expected?: string, actual?: string }
//
// 旧前端直接 String(e) 导致显示 [object Object]。本模块解析结构化字段,
// 供调用方按错误类型分级展示(warning vs error、重试按钮等)。

/** 后端 AppError 序列化格式 */
export interface AppErrorShape {
  type: string
  message: string
  /** Core 变体嵌套的 DownloadError(仅 type==="Core" 时存在) */
  inner?: DownloadErrorShape
}

/** 后端 DownloadError 序列化格式 */
export interface DownloadErrorShape {
  type: string
  message: string
  retryable: boolean
  retryAfterSecs?: number | null
  status?: number
  reason?: string
  expected?: string
  actual?: string
}

/** 归一化后的错误信息 */
export interface NormalizedError {
  /** 用户可读的错误消息(优先取 message 字段,兜底 String(e)) */
  message: string
  /** 错误类型(AppError.type 或 Core.inner.type) */
  type: string
  /** 是否可重试(仅 Core 变体有,其他默认 true) */
  retryable: boolean
  /** 限流等待秒数(仅 Throttled 变体) */
  retryAfterSecs?: number | null
  /** HTTP 状态码(Forbidden/Http 变体) */
  status?: number
  /** 原始 reject 值,供调试 */
  raw: unknown
}

/**
 * 解析 invoke reject 值为归一化错误。
 *
 * 处理三种输入:
 * 1. 后端 AppError 序列化对象(含 type/message/inner):读取结构化字段
 * 2. Error 实例:取 message,type 设为 "Unknown",retryable 默认 true
 * 3. 字符串/其他:转字符串作 message
 *
 * 优先解构 Core.inner(后端 14 种 DownloadError 变体),提供精确的
 * retryable/retryAfterSecs/status 字段;非 Core 变体回退到 AppError 级别。
 */
export function parseAppError(e: unknown): NormalizedError {
  // 情况 1:后端 AppError 序列化对象
  if (typeof e === 'object' && e !== null) {
    const appErr = e as Partial<AppErrorShape>
    if (typeof appErr.type === 'string' && typeof appErr.message === 'string') {
      // Core 变体:解构 inner 获取精确的 DownloadError 字段
      if (appErr.type === 'Core' && appErr.inner) {
        const inner = appErr.inner
        return {
          message: inner.message || appErr.message,
          type: inner.type,
          retryable: inner.retryable,
          retryAfterSecs: inner.retryAfterSecs ?? undefined,
          status: inner.status,
          raw: e,
        }
      }
      // 非 Core 变体:AppError 级别(Network/Config/TaskNotFound 等)
      // 这些变体的 retryable 语义:Network/Timeout 可重试,其余不可
      const retryableTypes = ['Network', 'Timeout']
      return {
        message: appErr.message,
        type: appErr.type,
        retryable: retryableTypes.includes(appErr.type),
        raw: e,
      }
    }
    // 对象但无 type/message 字段(异常情况):兜底 JSON.stringify
    const fallback = safeStringify(e)
    return { message: fallback, type: 'Unknown', retryable: true, raw: e }
  }

  // 情况 2:Error 实例
  if (e instanceof Error) {
    return { message: e.message, type: 'Unknown', retryable: true, raw: e }
  }

  // 情况 3:字符串/其他
  const msg = String(e ?? '')
  return { message: msg, type: 'Unknown', retryable: true, raw: e }
}

/** 安全的 JSON.stringify,处理循环引用 */
function safeStringify(obj: unknown): string {
  try {
    return JSON.stringify(obj)
  } catch {
    return String(obj)
  }
}

/**
 * 提取错误的可读消息(便捷方法)。
 *
 * 替代旧 `String(e)` 用法,避免显示 [object Object]。
 */
export function errorMessage(e: unknown): string {
  return parseAppError(e).message
}
