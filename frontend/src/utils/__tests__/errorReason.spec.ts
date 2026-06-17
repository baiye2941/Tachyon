import { describe, it, expect } from 'vitest'
import { isHuggingFaceUrl, parseHfUrl, inferFailure } from '../errorReason'
import type { TaskInfo } from '../../types'

/** 构造测试用 TaskInfo 的辅助函数 */
function makeTask(overrides: Partial<TaskInfo> = {}): TaskInfo {
  return {
    id: 'test-1',
    url: 'https://example.com/file.bin',
    fileName: 'file.bin',
    fileSize: 1024,
    downloaded: 0,
    speed: 0,
    status: 'failed',
    progress: 0,
    fragmentsTotal: 1,
    fragmentsDone: 0,
    createdAt: '2026-01-01T00:00:00Z',
    savePath: '/tmp/file.bin',
    ...overrides,
  }
}

describe('isHuggingFaceUrl — HuggingFace 链接识别(Iteration 16)', () => {
  it('主站 huggingface.co 链接', () => {
    expect(isHuggingFaceUrl('https://huggingface.co/bert-base/resolve/main/model.bin')).toBe(true)
    expect(isHuggingFaceUrl('http://huggingface.co/org/repo/resolve/main/file.gguf')).toBe(true)
  })

  it('www 前缀', () => {
    expect(isHuggingFaceUrl('https://www.huggingface.co/org/repo/resolve/main/file.bin')).toBe(true)
  })

  it('CDN 子域 cdn-lfs.huggingface.co', () => {
    expect(isHuggingFaceUrl('https://cdn-lfs.huggingface.co/bert-base/abc123/model.bin')).toBe(true)
  })

  it('hf-mirror.com 不是 HF 主站链接(已是镜像)', () => {
    expect(isHuggingFaceUrl('https://hf-mirror.com/bert-base/resolve/main/model.bin')).toBe(false)
  })

  it('非 HF 域名', () => {
    expect(isHuggingFaceUrl('https://example.com/file.bin')).toBe(false)
    expect(isHuggingFaceUrl('https://github.com/repo/file')).toBe(false)
  })

  it('空字符串/无效 URL', () => {
    expect(isHuggingFaceUrl('')).toBe(false)
    expect(isHuggingFaceUrl('not-a-url')).toBe(false)
  })
})

describe('parseHfUrl — HuggingFace URL 解析(Iteration 16)', () => {
  it('resolve 端点:解析 repoId/revision/filePath', () => {
    const result = parseHfUrl('https://huggingface.co/google-bert/bert-base-uncased/resolve/main/config.json')
    expect(result).toEqual({
      repoId: 'google-bert/bert-base-uncased',
      revision: 'main',
      filePath: 'config.json',
    })
  })

  it('带 owner 的 repoId', () => {
    const result = parseHfUrl('https://huggingface.co/meta-llama/Llama-3.2-1B/resolve/main/model.gguf')
    expect(result).toEqual({
      repoId: 'meta-llama/Llama-3.2-1B',
      revision: 'main',
      filePath: 'model.gguf',
    })
  })

  it('blob 端点:同样解析', () => {
    const result = parseHfUrl('https://huggingface.co/org/repo/blob/main/README.md')
    expect(result).toEqual({
      repoId: 'org/repo',
      revision: 'main',
      filePath: 'README.md',
    })
  })

  it('自定义 revision(tag)', () => {
    const result = parseHfUrl('https://huggingface.co/org/repo/resolve/v1.0.0/model.gguf')
    expect(result).toEqual({
      repoId: 'org/repo',
      revision: 'v1.0.0',
      filePath: 'model.gguf',
    })
  })

  it('深路径 filePath', () => {
    const result = parseHfUrl('https://huggingface.co/org/repo/resolve/main/deep/path/weights.bin')
    expect(result).toEqual({
      repoId: 'org/repo',
      revision: 'main',
      filePath: 'deep/path/weights.bin',
    })
  })

  it('CDN 子域 URL 返回 null(无法解析 repoId)', () => {
    expect(parseHfUrl('https://cdn-lfs.huggingface.co/bert-base/abc123/model.bin')).toBeNull()
  })

  it('非 HF URL 返回 null', () => {
    expect(parseHfUrl('https://example.com/file.bin')).toBeNull()
    expect(parseHfUrl('https://hf-mirror.com/org/repo/resolve/main/file.bin')).toBeNull()
  })

  it('空字符串返回 null', () => {
    expect(parseHfUrl('')).toBeNull()
  })
})

describe('inferFailure — 启发式推断增强(Iteration 16)', () => {
  it('cancelled 状态:返回 cancelled 分类', () => {
    const task = makeTask({ status: 'cancelled' })
    const insight = inferFailure(task)
    expect(insight.category).toBe('cancelled')
    expect(insight.retryable).toBe(true)
    expect(insight.canRetryWithMirror).toBeUndefined()
  })

  it('HF 链接 + downloaded=0:network 分类 + canRetryWithMirror', () => {
    const task = makeTask({
      url: 'https://huggingface.co/org/repo/resolve/main/model.gguf',
      downloaded: 0,
    })
    const insight = inferFailure(task)
    expect(insight.category).toBe('network')
    expect(insight.canRetryWithMirror).toBe(true)
    expect(insight.retryable).toBe(true)
  })

  it('HF 链接 + downloaded>0:interrupted + canRetryWithMirror', () => {
    const task = makeTask({
      url: 'https://huggingface.co/org/repo/resolve/main/model.gguf',
      downloaded: 512,
    })
    const insight = inferFailure(task)
    expect(insight.category).toBe('network')
    expect(insight.canRetryWithMirror).toBe(true)
  })

  it('非 HF 链接 + downloaded>0:interrupted + 无 canRetryWithMirror', () => {
    const task = makeTask({
      url: 'https://example.com/file.bin',
      downloaded: 512,
    })
    const insight = inferFailure(task)
    expect(insight.category).toBe('network')
    expect(insight.canRetryWithMirror).toBeUndefined()
  })

  it('非 HF 链接 + downloaded=0:unknown 回退', () => {
    const task = makeTask({
      url: 'https://example.com/file.bin',
      downloaded: 0,
    })
    const insight = inferFailure(task)
    expect(insight.category).toBe('unknown')
    expect(insight.retryable).toBe(true)
    expect(insight.canRetryWithMirror).toBeUndefined()
  })

  it('后端 errorReason 字段优先(Iteration 17 已暴露)', () => {
    const task = makeTask({
      url: 'https://example.com/file.bin',
      downloaded: 0,
      errorReason: 'Request timed out after 30s',
    })
    const insight = inferFailure(task)
    expect(insight.category).toBe('network')
    expect(insight.rawReason).toBe('Request timed out after 30s')
  })

  it('后端 errorReason + HF 链接:canRetryWithMirror 跟随 retryable', () => {
    const task = makeTask({
      url: 'https://huggingface.co/org/repo/resolve/main/model.gguf',
      downloaded: 0,
      errorReason: '404 Not Found',
    })
    const insight = inferFailure(task)
    // 404 → retryable=false → canRetryWithMirror=false
    expect(insight.retryable).toBe(false)
    expect(insight.canRetryWithMirror).toBe(false)
  })

  it('后端 errorReason 为空字符串:回退到启发式', () => {
    const task = makeTask({
      url: 'https://huggingface.co/org/repo/resolve/main/model.gguf',
      downloaded: 0,
      errorReason: '   ',
    })
    const insight = inferFailure(task)
    // 空字符串应回退到启发式,HF + downloaded=0 → network
    expect(insight.category).toBe('network')
    expect(insight.canRetryWithMirror).toBe(true)
  })

  it('后端 errorReason SSL 错误:分类为 ssl', () => {
    const task = makeTask({
      url: 'https://example.com/file.bin',
      errorReason: 'certificate verify failed: unable to get local issuer certificate',
    })
    const insight = inferFailure(task)
    expect(insight.category).toBe('ssl')
    expect(insight.retryable).toBe(true)
  })

  it('后端 errorReason 磁盘错误:分类为 disk', () => {
    const task = makeTask({
      url: 'https://example.com/file.bin',
      errorReason: 'No space left on device (os error 28)',
    })
    const insight = inferFailure(task)
    expect(insight.category).toBe('disk')
    expect(insight.retryable).toBe(true)
  })

  it('后端 errorReason 权限错误:分类为 auth', () => {
    const task = makeTask({
      url: 'https://example.com/file.bin',
      errorReason: 'HTTP 403 Forbidden',
    })
    const insight = inferFailure(task)
    expect(insight.category).toBe('auth')
    expect(insight.retryable).toBe(true)
  })
})
