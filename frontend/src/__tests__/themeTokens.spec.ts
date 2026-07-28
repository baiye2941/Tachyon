import { describe, it, expect } from 'vitest'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

// light 主题 token 单源(SSOT)回归测试:
// misc.css 曾与 tokens.css 存在逐行重复的 [data-theme="light"] 覆盖块且已漂移
// (--color-status-completed: misc 为 #0d9488,tokens 为 var(--color-brand-teal))。
// index.css 中 misc.css 在 tokens.css 之后加载,重复块会静默覆盖 SSOT,
// 故 light 覆盖只保留 tokens.css 一份,misc.css 不得再出现该选择器。

// vitest 的 cwd = frontend
const miscCssPath = join(process.cwd(), 'src/styles/components/misc.css')
const tokensCssPath = join(process.cwd(), 'src/styles/tokens.css')

const readCss = (path: string) => readFileSync(path, 'utf8')

// 提取裸 [data-theme="light"] { ... } 块(花括号配对),
// 不匹配 [data-theme="light"] .glass 等后代选择器块
function extractBareLightBlock(css: string): string | null {
  const match = /\[data-theme="light"\]\s*\{/.exec(css)
  if (!match) return null
  let depth = 0
  const start = match.index + match[0].length - 1 // 指向 '{'
  for (let i = start; i < css.length; i++) {
    if (css[i] === '{') {
      depth++
    } else if (css[i] === '}') {
      depth--
      if (depth === 0) return css.slice(match.index, i + 1)
    }
  }
  return null
}

describe('light 主题 token 单源(SSOT)', () => {
  it('misc.css 不再含 [data-theme="light"] 覆盖块(单源在 tokens.css)', () => {
    const miscCss = readCss(miscCssPath)
    expect(miscCss.includes('[data-theme="light"]')).toBe(false)
  })

  it('tokens.css 保留 [data-theme="light"] 裸选择器覆盖块', () => {
    const tokensCss = readCss(tokensCssPath)
    expect(tokensCss.includes('[data-theme="light"]')).toBe(true)
    expect(extractBareLightBlock(tokensCss)).not.toBeNull()
  })

  it('tokens.css light 裸块内含 --color-status-completed', () => {
    const block = extractBareLightBlock(readCss(tokensCssPath))
    expect(block).not.toBeNull()
    expect(block).toContain('--color-status-completed')
  })

  it('漂移仲裁: --color-status-completed 以 tokens 为准(var(--color-brand-teal))', () => {
    const block = extractBareLightBlock(readCss(tokensCssPath))
    expect(block).not.toBeNull()
    expect(block).toContain('--color-status-completed: var(--color-brand-teal)')
  })
})
