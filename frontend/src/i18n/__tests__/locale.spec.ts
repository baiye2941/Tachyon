import { describe, it, expect } from 'vitest'
import zhCN from '../../i18n/locales/zh-CN'
import enUS from '../../i18n/locales/en-US'

/**
 * i18n 完整性测试(Iteration 14)
 *
 * 验证:
 * 1. zh-CN 与 en-US 的 key 集合完全对等(缺一即漏译)
 * 2. 每个 key 的 en-US 值不含中文字符(英译不应残留中文)
 * 3. 每个 key 的值非空字符串
 */
describe('i18n locale 完整性', () => {
  const zhKeys = Object.keys(zhCN)
  const enKeys = Object.keys(enUS)

  it('zh-CN 与 en-US key 数量一致', () => {
    expect(zhKeys.length).toBe(enKeys.length)
  })

  it('zh-CN 与 en-US key 集合完全对等', () => {
    const zhSet = new Set(zhKeys)
    const enSet = new Set(enKeys)
    const missingInEn = zhKeys.filter((k) => !enSet.has(k))
    const missingInZh = enKeys.filter((k) => !zhSet.has(k))
    expect(missingInEn, `en-US 缺失的 key: ${missingInEn.join(', ')}`).toEqual([])
    expect(missingInZh, `zh-CN 缺失的 key: ${missingInZh.join(', ')}`).toEqual([])
  })

  it('每个 key 的值非空', () => {
    for (const [key, val] of Object.entries(zhCN)) {
      expect(val, `zh-CN key "${key}" 值为空`).toBeTruthy()
    }
    for (const [key, val] of Object.entries(enUS)) {
      expect(val, `en-US key "${key}" 值为空`).toBeTruthy()
    }
  })

  it('en-US 翻译不残留中文字符', () => {
    const chineseRe = /[\u4e00-\u9fff]/
    for (const [key, val] of Object.entries(enUS)) {
      expect(
        chineseRe.test(val),
        `en-US key "${key}" 残留中文: "${val}"`,
      ).toBe(false)
    }
  })

  it('zh-CN 翻译包含中文字符(确保非空壳)', () => {
    const chineseRe = /[\u4e00-\u9fff]/
    let hasChinese = 0
    for (const val of Object.values(zhCN)) {
      if (chineseRe.test(val)) hasChinese++
    }
    // 至少 90% 的 zh-CN 值应含中文(允许少数如 "Tachyon" 等品牌词)
    expect(hasChinese / zhKeys.length).toBeGreaterThanOrEqual(0.9)
  })
})
