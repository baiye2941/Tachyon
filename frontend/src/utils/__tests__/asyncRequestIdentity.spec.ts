import { describe, it, expect } from 'vitest'
import {
  nextRequestSeq,
  shouldApplyProbeResult,
  shouldApplyHfPreview,
  resolveOptimisticConfigOnFailure,
} from '../asyncRequestIdentity'

describe('asyncRequestIdentity FT-10', () => {
  it('nextRequestSeq 单调递增', () => {
    expect(nextRequestSeq(0)).toBe(1)
    expect(nextRequestSeq(41)).toBe(42)
  })

  it('陈旧 probe seq 不应用', () => {
    expect(shouldApplyProbeResult(1, 2, 'https://a', 'https://a')).toBe(false)
  })

  it('URL 已切换时不应用旧 probe', () => {
    expect(shouldApplyProbeResult(1, 1, 'https://a', 'https://b')).toBe(false)
  })

  it('seq 与 URL 均匹配时应用 probe', () => {
    expect(shouldApplyProbeResult(3, 3, 'https://a', 'https://a')).toBe(true)
  })

  it('HF 预览 repo 不一致丢弃', () => {
    expect(
      shouldApplyHfPreview(1, 1, { repoId: 'org/a' }, { repoId: 'org/b' }),
    ).toBe(false)
  })

  it('HF 预览 revision 不一致丢弃', () => {
    expect(
      shouldApplyHfPreview(
        1,
        1,
        { repoId: 'org/a', revision: 'main' },
        { repoId: 'org/a', revision: 'v1' },
      ),
    ).toBe(false)
  })

  it('HF 预览匹配时应用', () => {
    expect(
      shouldApplyHfPreview(2, 2, { repoId: 'org/a' }, { repoId: 'org/a' }),
    ).toBe(true)
  })

  it('stale seq 丢弃 HF 预览', () => {
    expect(
      shouldApplyHfPreview(1, 9, { repoId: 'org/a' }, { repoId: 'org/a' }),
    ).toBe(false)
  })
})

describe('asyncRequestIdentity FT-11', () => {
  it('失败回滚 previous', () => {
    const prev = { enabledTypes: ['video'] as string[], minSize: 0, urlFilters: [] as string[] }
    const next = { enabledTypes: ['audio'], minSize: 1, urlFilters: ['x'] }
    expect(resolveOptimisticConfigOnFailure(prev, next, true)).toEqual(prev)
  })

  it('成功保留 attempted', () => {
    const prev = { a: 1 }
    const next = { a: 2 }
    expect(resolveOptimisticConfigOnFailure(prev, next, false)).toEqual(next)
  })
})
