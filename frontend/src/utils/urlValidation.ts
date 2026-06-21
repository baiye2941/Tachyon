/**
 * URL 校验与协议识别。
 *
 * 在前端做基础校验,减少无效请求到达后端的往返延迟。
 * 后端仍会做最终授权与解析(纵深防御),前端校验仅用于即时反馈。
 */

/** 识别到的链接协议类型 */
export type UrlProtocol =
  | 'http'
  | 'https'
  | 'ftp'
  | 'huggingface'
  | 'magnet'
  | 'unknown'

export interface UrlValidation {
  /** 是否为可下载的有效链接 */
  valid: boolean
  /** 协议类型 */
  protocol: UrlProtocol
  /** 额外提示的 i18n key(调用方用 tr() 翻译) */
  hintKey?: 'url.hint.magnet.resolving' | 'url.hint.huggingface' | 'url.hint.invalid'
}

const HTTP_RE = /^https?:\/\/[^\s/$.?#].[^\s]*$/i
const FTP_RE = /^ftp:\/\/[^\s/$.?#].[^\s]*$/i
const HF_RE = /^https?:\/\/(www\.)?huggingface\.co\//i
const MAGNET_RE = /^magnet:\?xt=urn:btih:/i

/**
 * 校验单个 URL 字符串。
 *
 * 识别逻辑:
 * - magnet: 磁力链接,有效(通过 BitTorrent 协议下载)
 * - huggingface: HuggingFace 链接,有效(为 Iteration 06 HF Provider 铺路)
 * - http/https/ftp: 标准协议,有效
 * - 其他: invalid
 */
export function validateUrl(raw: string): UrlValidation {
  const trimmed = raw.trim()
  if (!trimmed) {
    return { valid: false, protocol: 'unknown' }
  }

  if (MAGNET_RE.test(trimmed)) {
    return {
      valid: true,
      protocol: 'magnet',
    }
  }

  if (HF_RE.test(trimmed)) {
    return {
      valid: true,
      protocol: 'huggingface',
      hintKey: 'url.hint.huggingface',
    }
  }

  if (HTTP_RE.test(trimmed)) {
    return {
      valid: true,
      protocol: trimmed.startsWith('https') ? 'https' : 'http',
    }
  }

  if (FTP_RE.test(trimmed)) {
    return { valid: true, protocol: 'ftp' }
  }

  return {
    valid: false,
    protocol: 'unknown',
    hintKey: 'url.hint.invalid',
  }
}

export interface ParsedUrlLine {
  /** 原始行 */
  raw: string
  /** 校验结果 */
  validation: UrlValidation
}

/**
 * 解析多行文本为 URL 行数组(供 textarea 批量输入)。
 *
 * 兼容 CRLF/LF 换行,忽略空行与 # 注释行。
 */
export function parseUrlLines(text: string): ParsedUrlLine[] {
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0 && !line.startsWith('#'))
    .map((raw) => ({ raw, validation: validateUrl(raw) }))
}
