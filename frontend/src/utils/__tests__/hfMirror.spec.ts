import { describe, it, expect } from 'vitest'
import { buildHfMirrorUrl } from '../hfMirror'

describe('buildHfMirrorUrl — 镜像鲁棒化(Iteration 06 PF-3)', () => {
  it('普通文件:构造 hf-mirror resolve URL', () => {
    expect(buildHfMirrorUrl('bert-base-uncased', 'main', 'config.json')).toBe(
      'https://hf-mirror.com/bert-base-uncased/resolve/main/config.json',
    )
  })

  it('带 owner 的 repoId', () => {
    expect(buildHfMirrorUrl('meta-llama/Llama-3.2-1B', 'main', 'model.gguf')).toBe(
      'https://hf-mirror.com/meta-llama/Llama-3.2-1B/resolve/main/model.gguf',
    )
  })

  it('LFS 文件:统一走 resolve 端点(绕过 cdn-lfs 域名差异)', () => {
    const url = buildHfMirrorUrl('org/repo', 'main', 'model.safetensors')
    expect(url).toBe('https://hf-mirror.com/org/repo/resolve/main/model.safetensors')
    // 不应出现 cdn-lfs / cas-bridge 等后端 CDN 域名
    expect(url).not.toContain('cdn-lfs')
    expect(url).not.toContain('cas-bridge')
    expect(url).not.toContain('xethub')
  })

  it('Xet 架构文件:同样走 resolve 端点(绕过 cas-bridge 域名)', () => {
    const url = buildHfMirrorUrl('org/repo', 'main', 'deep/path/weights.bin')
    expect(url).toBe('https://hf-mirror.com/org/repo/resolve/main/deep/path/weights.bin')
  })

  it('自定义 revision', () => {
    expect(buildHfMirrorUrl('org/repo', 'v1.0.0', 'model.gguf')).toBe(
      'https://hf-mirror.com/org/repo/resolve/v1.0.0/model.gguf',
    )
  })

  it('空 revision 回退 main', () => {
    expect(buildHfMirrorUrl('org/repo', '', 'model.gguf')).toBe(
      'https://hf-mirror.com/org/repo/resolve/main/model.gguf',
    )
  })

  it('文件路径含空格/特殊字符:encodeURIComponent 编码各段', () => {
    expect(buildHfMirrorUrl('org/repo', 'main', 'my file.gguf')).toBe(
      'https://hf-mirror.com/org/repo/resolve/main/my%20file.gguf',
    )
    expect(buildHfMirrorUrl('org/repo', 'main', 'a/b c/d.gguf')).toBe(
      'https://hf-mirror.com/org/repo/resolve/main/a/b%20c/d.gguf',
    )
  })

  it('路径分隔符 / 不被编码(保持目录层级)', () => {
    const url = buildHfMirrorUrl('org/repo', 'main', 'sub/dir/file.gguf')
    expect(url).toContain('sub/dir/file.gguf')
    expect(url).not.toContain('sub%2F')
  })
})
