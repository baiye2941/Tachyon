/**
 * 硬编码颜色守护脚本。
 *
 * 扫描 frontend/src 下的 .ts/.tsx 文件,检测直接使用的十六进制颜色值
 * (如 #8B5CF6),这些应替换为 var(--color-*) semantic token。
 *
 * 例外白名单:
 * - index.css:token 定义文件,允许原始十六进制。
 * - format.ts 的 THREAD_COLORS:Canvas 场景无法用 var(),保留字面值并附 token 映射注释。
 * - resolveToken.ts:fallback 颜色,本身是 token 解析器。
 *
 * 用法: bun run scripts/lint-color-hardcode.ts
 * 退出码: 发现违规返回 1,无违规返回 0。
 */
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, relative, sep } from 'node:path'

const ROOT = join(import.meta.dir, '..', 'src')
const HEX_RE = /#[0-9A-Fa-f]{6}\b/g
/**
 * 检测内联 rgba()/rgb() 颜色(Iteration 06 增强)。
 * 玻璃拟态(backdrop-filter)与硬编码阴影常以 rgba(0,0,0,...) 形式出现,
 * 这些应使用 var(--shadow-*) / var(--color-border-*) 等 token。
 * 仅匹配「值上下文」中的 rgba/rgb(跳过 CSS 变量定义文件 index.css 已在白名单)。
 */
const RGB_RE = /\brgba?\(\s*\d/g

/** 允许包含十六进制颜色的文件(相对 src 的 POSIX 路径) */
const ALLOW_FILES = new Set<string>([
  'index.css', // token 定义文件,允许原始十六进制
  'utils/format.ts', // THREAD_COLORS 数组(Canvas 场景,附 token 映射注释)
  'utils/resolveToken.ts', // fallback 色,本身是 token 解析器
  'utils/stateMachine.ts', // 死代码(审查 P3-2),待删除,暂豁免
  'components/__tests__/Accessibility.spec.tsx', // 对比度自验证测试
])

function toPosix(p: string): string {
  return p.split(sep).join('/')
}

function walk(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry)
    const st = statSync(full)
    if (st.isDirectory()) {
      walk(full, out)
    } else if (/\.(tsx?|css)$/.test(entry)) {
      out.push(full)
    }
  }
  return out
}

const files = walk(ROOT)
const violations: { file: string; line: number; value: string }[] = []

for (const file of files) {
  const rel = toPosix(relative(ROOT, file))
  if (ALLOW_FILES.has(rel)) continue

  const content = readFileSync(file, 'utf8')
  const lines = content.split('\n')
  lines.forEach((line, i) => {
    // 跳过注释行(以 // 或 * 开头)
    const trimmed = line.trim()
    if (trimmed.startsWith('//') || trimmed.startsWith('*')) return

    const matches = line.match(HEX_RE)
    if (matches) {
      for (const value of matches) {
        violations.push({ file: rel, line: i + 1, value })
      }
    }
    // rgba/rgb 内联颜色检测(防止玻璃拟态/硬编码阴影回潮)
    if (RGB_RE.test(line)) {
      const m = line.match(/\brgba?\([^)]*\)/)
      violations.push({ file: rel, line: i + 1, value: m ? m[0] : 'rgba(...)' })
    }
  })
}

if (violations.length > 0) {
  console.error('❌ 发现硬编码颜色,请使用 var(--color-*) semantic token:\n')
  for (const v of violations) {
    console.error(`  ${v.file}:${v.line}  ${v.value}`)
  }
  console.error(`\n共 ${violations.length} 处。`)
  process.exit(1)
}

console.log('✅ 无硬编码颜色(DOM 场景)或所有十六进制均在白名单内')
