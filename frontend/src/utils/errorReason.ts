import type { TaskInfo } from '../types'

/**
 * 失败任务的诊断信息。
 *
 * 后端 TaskInfo 暂无 errorReason 字段,前端用启发式推断 + 容错降级。
 * 当后端添加 error_reason 字段后,parseExplicitError 会自动优先使用真实原因。
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
}

/**
 * 后端 error_reason 字段的解析(当后端支持后自动启用)。
 *
 * 后端返回的错误字符串可能包含 HTTP 状态码、超时、IO 错误等。
 * 这里做基础分类,未匹配的回退到 unknown。
 */
function parseExplicitError(reason: string): FailureInsight {
  const lower = reason.toLowerCase()
  // HTTP 状态码类
  if (/401|403|unauthor|forbidden/.test(lower)) {
    return {
      category: 'auth',
      title: '访问被拒绝',
      hint: '链接可能需要登录或已失效,请检查权限或更换镜像源',
      retryable: true,
    }
  }
  if (/404|not found/.test(lower)) {
    return {
      category: 'network',
      title: '资源不存在',
      hint: '链接已失效(404),请确认链接正确或更换镜像源',
      retryable: false,
    }
  }
  if (/timeout|timed out/.test(lower)) {
    return {
      category: 'network',
      title: '连接超时',
      hint: '网络响应过慢,请检查网络后重试或更换镜像源',
      retryable: true,
    }
  }
  if (/ssl|tls|certificate/.test(lower)) {
    return {
      category: 'ssl',
      title: '证书错误',
      hint: 'SSL/TLS 证书校验失败,可能是系统时间错误或中间人攻击',
      retryable: true,
    }
  }
  if (/disk|space|enospc|no space/.test(lower)) {
    return {
      category: 'disk',
      title: '磁盘空间不足',
      hint: '保存目录所在磁盘已满,请清理空间或更换保存目录',
      retryable: true,
    }
  }
  // 未匹配,回退但保留真实原因供用户参考
  return {
    category: 'unknown',
    title: '下载失败',
    hint: reason,
    retryable: true,
  }
}

/**
 * 基于任务信息推断失败原因。
 *
 * 优先级:
 * 1. 后端 errorReason 字段(若存在,自动启用)
 * 2. cancelled 状态特殊处理
 * 3. 启发式推断(URL 协议等)
 * 4. 诚实回退到 unknown
 */
export function inferFailure(task: TaskInfo): FailureInsight {
  // 1. 后端字段优先(类型扩展,运行时检测)
  const explicit = (task as TaskInfo & { errorReason?: string }).errorReason
  if (explicit && explicit.trim().length > 0) {
    return parseExplicitError(explicit)
  }

  // 2. cancelled 状态
  if (task.status === 'cancelled') {
    return {
      category: 'cancelled',
      title: '已取消',
      hint: '任务被手动取消,可重新下载',
      retryable: true,
    }
  }

  // 3. 启发式推断:基于已下载量判断是否曾建立连接
  if (task.downloaded > 0) {
    return {
      category: 'network',
      title: '下载中断',
      hint: '连接在传输过程中断开,可断点续传重试',
      retryable: true,
    }
  }

  // 4. 诚实回退:不假装知道原因
  return {
    category: 'unknown',
    title: '下载失败',
    hint: '原因未知。点击「重试」重新下载,若持续失败请检查网络、磁盘空间或更换镜像源',
    retryable: true,
  }
}
