import { describe, it, expect } from 'vitest'
import { fuzzyMatch, fuzzySearch } from '../fuzzySearch'

describe('fuzzyMatch', () => {
  it('空 query 返回 score 0(视为匹配全部)', () => {
    expect(fuzzyMatch('', '下载管理').score).toBe(0)
  })

  it('完全前缀匹配得高分', () => {
    const r = fuzzyMatch('下载', '下载管理')
    expect(r.score).toBeGreaterThan(0)
    expect(r.indices).toEqual([0, 1])
  })

  it('子序列匹配(非连续)成功', () => {
    // 'pa' 匹配 'pause-all' 的 P-a
    const r = fuzzyMatch('pa', 'pause-all')
    expect(r.score).toBeGreaterThan(0)
    expect(r.indices).toEqual([0, 1])
  })

  it('子序列匹配中文缩写', () => {
    // '导航' 的子序列
    const r = fuzzyMatch('导航', '资源嗅探导航')
    expect(r.score).toBeGreaterThan(0)
  })

  it('非子序列返回 -1', () => {
    expect(fuzzyMatch('xyz', '下载管理').score).toBe(-1)
  })

  it('query 比 target 长返回 -1', () => {
    expect(fuzzyMatch('abc', 'a').score).toBe(-1)
  })

  it('连续匹配比分散匹配得分高', () => {
    const continuous = fuzzyMatch('nav', 'navigation')
    const scattered = fuzzyMatch('nav', 'n___a___v')
    expect(continuous.score).toBeGreaterThan(scattered.score)
  })

  it('前缀匹配比非前缀得分高', () => {
    const prefix = fuzzyMatch('set', 'settings')
    const nonPrefix = fuzzyMatch('set', 'offset')
    expect(prefix.score).toBeGreaterThan(nonPrefix.score)
  })

  it('大小写不敏感', () => {
    expect(fuzzyMatch('DL', 'download').score).toBeGreaterThan(0)
    expect(fuzzyMatch('dl', 'DOWNLOAD').score).toBeGreaterThan(0)
  })

  it('词首匹配加分(空格后)', () => {
    const r = fuzzyMatch('all', 'pause all')
    expect(r.score).toBeGreaterThan(0)
  })
})

describe('fuzzySearch', () => {
  const items = [
    { id: 1, label: '下载管理', hint: '查看所有下载任务' },
    { id: 2, label: '全部暂停', hint: '暂停所有下载' },
    { id: 3, label: '设置', hint: '应用配置' },
  ]
  const getText = (i: { label: string; hint: string }) => `${i.label} ${i.hint}`

  it('空 query 返回全部项(原序, score 0)', () => {
    const r = fuzzySearch(items, '', getText)
    expect(r.length).toBe(3)
    expect(r.every((x) => x.score === 0)).toBe(true)
  })

  it('过滤非匹配项', () => {
    const r = fuzzySearch(items, 'xyz', getText)
    expect(r.length).toBe(0)
  })

  it('按 score 降序排序', () => {
    // '设置' 精确匹配 items[2],score 最高
    const r = fuzzySearch(items, '设置', getText)
    expect(r.length).toBeGreaterThan(0)
    expect(r[0]!.item.id).toBe(3)
  })

  it('hint 中的匹配也被搜到', () => {
    // '暂停' 在 items[2] 无,在 items[1] label 有
    const r = fuzzySearch(items, '暂停', getText)
    expect(r.some((x) => x.item.id === 2)).toBe(true)
  })

  it('提供 matchedIndices 用于高亮', () => {
    const r = fuzzySearch(items, '下载', getText)
    expect(r[0]!.matchedIndices.length).toBeGreaterThan(0)
  })
})
