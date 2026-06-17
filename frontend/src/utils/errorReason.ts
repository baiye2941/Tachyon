import type { TaskInfo } from '../types'
import { tr } from '../i18n'

/**
 * 失败任务的诊断信息。
 *
 * 后端 TaskInfo 已暴露 errorReason 字段(Iteration 17 完成),
 * inferFailure 优先使用后端原文,无后端原文时回退到启发式推断。
 *
 * 设计原则(Iteration 01 信任原则的延伸):
 * 宁可显示「失败原因未知,点击重试」这种诚实的不确定,
 * 也不显示假的「连接超时」(原 DetailPanel 的硬编码假错误)。
 */
export interface FailureInsight {
  /** 错误分类,用于决定恢复建议 */
  category: 'network' | 'auth' | 'disk' | 'ssl' | 'cancelled' | 'unknown'
  /** 错误标题(简短) */
  title: string
  /** 错误详情 / 恢复建议(可操作) */
  hint: string
  /** 是否值得重试 */
  retryable: boolean
  /** 后端返回的原始错误字符串(当 errorReason 字段存在时保留,供诊断面板展开展示) */
  rawReason?: string
  /** 是否可通过镜像源重试(HuggingFace 链接失败时为 true) */
  canRetryWithMirror?: boolean
}

/**
 * 后端 error_reason 字段的解析(当后端支持后自动启用)。
 *
 * 后端返回的错误字符串可能包含 HTTP 状态码、超时、IO 错误等。
 * 这里做基础分类,未匹配的回退到 unknown。
 */
function parseExplicitError(reason: string): FailureInsight {
  const lower = reason.toLowerCase()
  let insight: Omit<FailureInsight, 'rawReason'>
  // HTTP 状态码类
  if (/401|403|unauthor|forbidden/.test(lower)) {
    insight = {
      category: 'auth',
      title: tr('error.title.accessDenied'),
      hint: tr('error.hint.accessDenied'),
      retryable: true,
    }
  } else if (/404|not found/.test(lower)) {
    insight = {
      category: 'network',
      title: tr('error.title.notFound'),
      hint: tr('error.hint.notFound'),
      retryable: false,
    }
  } else if (/timeout|timed out/.test(lower)) {
    insight = {
      category: 'network',
      title: tr('error.title.timeout'),
      hint: tr('error.hint.timeout'),
      retryable: true,
    }
  } else if (/ssl|tls|certificate/.test(lower)) {
    insight = {
      category: 'ssl',
      title: tr('error.title.ssl'),
      hint: tr('error.hint.ssl'),
      retryable: true,
    }
  } else if (/disk|space|enospc|no space/.test(lower)) {
    insight = {
      category: 'disk',
      title: tr('error.title.diskFull'),
      hint: tr('error.hint.diskFull'),
      retryable: true,
    }
  } else {
    // 未匹配,回退但保留真实原因供用户参考
    insight = {
      category: 'unknown',
      title: tr('error.title.downloadFailed'),
      hint: reason,
      retryable: true,
    }
  }
  return { ...insight, rawReason: reason }
}

/**
 * 判断 URL 是否为 HuggingFace 链接。
 *
 * 匹配 huggingface.co 主站及 cdn-lfs.huggingface.co 等 CDN 子域。
 * 不匹配 hf-mirror.com(已是镜像)。
 */
export function isHuggingFaceUrl(url: string): boolean {
  return /^https?:\/\/([^/]*\.)?huggingface\.co\//i.test(url)
}

/**
 * 从 HuggingFace URL 解析 repoId / revision / filePath。
 *
 * 支持两种 URL 格式:
 * - https://huggingface.co/{repoId}/resolve/{revision}/{filePath}
 * - https://huggingface.co/{repoId}/blob/{revision}/{filePath}
 *
 * CDN 重定向 URL(cdn-lfs.huggingface.co/...)无法解析,返回 null。
 *
 * @returns 解析失败返回 null;成功返回 { repoId, revision, filePath }
 */
export function parseHfUrl(
  url: string,
): { repoId: string; revision: string; filePath: string } | null {
  // 仅匹配主站 huggingface.co,不匹配 CDN 子域
  const match = url.match(
    /^https?:\/\/(?:www\.)?huggingface\.co\/([^/]+\/[^/]+)\/(?:resolve|blob)\/([^/]+)\/(.+)$/i,
  )
  if (!match) return null
  const [, repoId, revision, filePath] = match
  if (!repoId || !revision || !filePath) return null
  return { repoId, revision, filePath }
}

/**
 * 基于任务信息推断失败原因。
 *
 * 优先级:
 * 1. 后端 errorReason 字段(Iteration 17 已暴露,直接使用)
 * 2. cancelled 状态特殊处理
 * 3. 启发式推断:URL 协议 + 已下载量
 * 4. 诚实回退到 unknown
 */
export function inferFailure(task: TaskInfo): FailureInsight {
  // 1. 后端字段优先(Iteration 17 后端已暴露 errorReason)
  const explicit = task.errorReason
  if (explicit && explicit.trim().length > 0) {
    const insight = parseExplicitError(explicit)
    // 后端错误 + HF 链接 → 额外标记可镜像重试
    if (task.url && isHuggingFaceUrl(task.url)) {
      insight.canRetryWithMirror = insight.retryable
    }
    return insight
  }

  // 2. cancelled 状态
  if (task.status === 'cancelled') {
    return {
      category: 'cancelled',
      title: tr('error.title.cancelled'),
      hint: tr('error.hint.cancelled'),
      retryable: true,
    }
  }

  // 3. 启发式推断:基于 URL 协议 + 已下载量
  const isHf = task.url ? isHuggingFaceUrl(task.url) : false

  if (task.downloaded > 0) {
    // 已建立连接并传输过数据 → 网络中断
    return {
      category: 'network',
      title: tr('error.title.interrupted'),
      hint: isHf
        ? tr('error.hint.interruptedHf')
        : tr('error.hint.interrupted'),
      retryable: true,
      // 仅 HF 链接标记可镜像重试;非 HF 链接不设置此字段(undefined)
      ...(isHf ? { canRetryWithMirror: true } : {}),
    }
  }

  // 未建立连接(downloaded=0)
  if (isHf) {
    // HF 链接 + 未下载 → 大概率网络/镜像问题
    return {
      category: 'network',
      title: tr('error.title.timeout'),
      hint: tr('error.hint.huggingfaceNetwork'),
      retryable: true,
      canRetryWithMirror: true,
    }
  }

  // 4. 诚实回退:不假装知道原因
  return {
    category: 'unknown',
    title: tr('error.title.downloadFailed'),
    hint: tr('error.hint.unknown'),
    retryable: true,
  }
}
