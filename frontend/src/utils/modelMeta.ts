/**
 * AI 模型文件元信息识别。
 *
 * 识别 GGUF/Safetensors/PT/ONNX 等模型格式,以及 GGUF 量化等级
 * (Q4_K_M / Q8_0 / F16 等),供 HubPanel 文件树标注。
 *
 * 量化等级命名遵循 llama.cpp 约定,详见:
 * https://github.com/ggerganov/llama.cpp/blob/master/examples/quantize/README.md
 */

/** 模型文件格式 */
export type ModelFormat = 'gguf' | 'safetensors' | 'pytorch' | 'onnx' | 'other'

/** GGUF 量化等级(粗精度排序,数字越小质量越低、体积越小) */
export interface QuantLevel {
  /** 标签(显示用),如 "Q4_K_M" */
  label: string
  /** 精度排序,1-10(用于 UI 排序,非精确 bits) */
  rank: number
  /** 体积档位(相对),用于快速筛选 */
  tier: 'tiny' | 'small' | 'medium' | 'large'
}

const QUANT_MAP: Record<string, QuantLevel> = {
  // 极低量化
  'q2_k': { label: 'Q2_K', rank: 1, tier: 'tiny' },
  'q3_k_s': { label: 'Q3_K_S', rank: 2, tier: 'tiny' },
  'q3_k_m': { label: 'Q3_K_M', rank: 3, tier: 'small' },
  'q3_k_l': { label: 'Q3_K_L', rank: 4, tier: 'small' },
  'q4_0': { label: 'Q4_0', rank: 5, tier: 'small' },
  // 主流平衡点
  'q4_k_s': { label: 'Q4_K_S', rank: 6, tier: 'small' },
  'q4_k_m': { label: 'Q4_K_M', rank: 6, tier: 'medium' },
  'q5_0': { label: 'Q5_0', rank: 7, tier: 'medium' },
  'q5_k_s': { label: 'Q5_K_S', rank: 8, tier: 'medium' },
  'q5_k_m': { label: 'Q5_K_M', rank: 8, tier: 'medium' },
  'q6_k': { label: 'Q6_K', rank: 9, tier: 'large' },
  // 高精度
  'q8_0': { label: 'Q8_0', rank: 10, tier: 'large' },
  'f16': { label: 'F16', rank: 11, tier: 'large' },
  'f32': { label: 'F32', rank: 12, tier: 'large' },
}

/**
 * 从文件名提取 GGUF 量化等级。
 *
 * 文件名常见格式:
 * - model-Q4_K_M.gguf
 * - qwen2.5-7b-instruct-q5_k_m.gguf
 * 匹配大小写不敏感,提取 qN_X_M / fN 等模式。
 */
export function detectQuant(fileName: string): QuantLevel | null {
  const lower = fileName.toLowerCase()
  // 优先匹配带变体的(_k_m / _k_s / _k_l)
  const m = lower.match(/(q\d+_[kslm]+|q\d+|f\d+)/)
  if (!m || !m[1]) return null
  return QUANT_MAP[m[1]] ?? null
}

/**
 * 识别模型文件格式。
 */
export function detectFormat(fileName: string): ModelFormat {
  const ext = fileName.split('.').pop()?.toLowerCase()
  if (ext === 'gguf') return 'gguf'
  if (ext === 'safetensors') return 'safetensors'
  if (ext === 'pt' || ext === 'pth' || ext === 'bin') return 'pytorch'
  if (ext === 'onnx') return 'onnx'
  return 'other'
}

/**
 * 判断文件是否为 AI 模型权重文件(用于 HubPanel 高亮)。
 */
export function isModelWeight(fileName: string): boolean {
  const format = detectFormat(fileName)
  return format === 'gguf' || format === 'safetensors' || format === 'pytorch' || format === 'onnx'
}

/** 常见模型仓库的「非权重」文件(配置/tokenizer),通常体积小,选择性下载时默认勾选 */
export const SMALL_SUPPORT_FILES = new Set([
  'config.json',
  'tokenizer.json',
  'tokenizer_config.json',
  'vocab.json',
  'merges.txt',
  'special_tokens_map.json',
  'generation_config.json',
  'model.safetensors.index.json',
  'README.md',
])

/**
 * 给定文件大小,判断是否为「大文件」(通常需要选择性下载决策)。
 * 阈值 100MB。
 */
export function isLargeFile(size: number): boolean {
  return size >= 100 * 1024 * 1024
}
