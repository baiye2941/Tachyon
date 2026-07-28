/// <reference types="node" />
/**
 * 硬编码颜色守护脚本。
 *
 * 扫描 frontend/src 下的 .ts/.tsx/.css 文件,检测直接使用的十六进制颜色值
 * (3~8 位,如 #fff / #8B5CF6 / #8B5CF6FF)与内联 rgb()/rgba(),
 * 这些应替换为 var(--color-*) semantic token。
 *
 * 例外白名单:
 * - styles/ 目录下所有 CSS:token 定义与组件样式实体所在,允许原始颜色值。
 * - index.css:样式入口文件,允许原始十六进制。
 * - utils/format.ts 的 THREAD_COLORS:Canvas 场景无法用 var(),保留字面值并附 token 映射注释。
 * - utils/resolveToken.ts:fallback 颜色,本身是 token 解析器。
 * - components/ChunkMatrix.tsx:Canvas fillStyle/渐变无法引用 var(),颜色已走 resolveToken 机制。
 * - 颜色断言/对比度算法自验证测试:颜色字面量是断言对象或算法输入本身。
 *
 * 用法: bun run scripts/lint-color-hardcode.ts
 * 退出码: 发现违规返回 1,无违规返回 0。
 */
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, join, relative, sep } from 'node:path'
import { fileURLToPath } from 'node:url'

/** 匹配 3~8 位十六进制颜色;\b 边界防止 9 位以上连续 hex 被截断误判 */
export const HEX_RE = /#[0-9A-Fa-f]{3,8}\b/g
/**
 * 检测内联 rgba()/rgb() 颜色(Iteration 06 增强)。
 * 玻璃拟态(backdrop-filter)与硬编码阴影常以 rgba(0,0,0,...) 形式出现,
 * 这些应使用 var(--shadow-*) / var(--color-border-*) 等 token。
 * 仅匹配「值上下文」中的 rgba/rgb。
 * 注意:不带 /g。RegExp.test 在 /g 下经 lastIndex 记忆位置,
 * 跨行复用会导致相邻行交替漏检(曾漏检 ChunkMatrix 相邻渐变行)。
 */
export const RGB_RE = /\brgba?\(\s*\d/

/** 允许包含颜色字面量的文件(相对 src 的 POSIX 路径) */
const ALLOW_FILES = new Set<string>([
  'index.css', // 样式入口文件,允许原始十六进制
  'utils/format.ts', // THREAD_COLORS 数组(Canvas 场景,附 token 映射注释)
  'utils/resolveToken.ts', // fallback 色,本身是 token 解析器
  'components/ChunkMatrix.tsx', // Canvas fillStyle/渐变无法用 var(),颜色已走 resolveToken 机制
  'components/__tests__/Accessibility.spec.tsx', // 对比度自验证测试
  'components/__tests__/ChunkMatrix.spec.tsx', // Canvas 颜色断言,字面量是断言对象
  'test-utils/__tests__/compositeContrast.spec.ts', // 颜色合成算法自验证,字面量是算法输入
])

/**
 * 判断相对 src 的 POSIX 路径是否豁免扫描。
 * styles/ 目录下所有 CSS(token 定义与组件样式实体)一律豁免。
 */
export function isAllowlisted(rel: string): boolean {
  if (ALLOW_FILES.has(rel)) return true
  return rel.startsWith('styles/') && rel.endsWith('.css')
}

/**
 * 判断是否为注释行。
 * 覆盖 // 行注释、/* 块注释起始、* 块注释续行、{/* JSX 注释。
 * JSX 注释里的 HTML 实体(如 &#10003;)含 5 位 hex 形态,不识别会误报。
 */
export function isCommentLine(line: string): boolean {
  const trimmed = line.trim()
  return (
    trimmed.startsWith('//') ||
    trimmed.startsWith('/*') ||
    trimmed.startsWith('*') ||
    trimmed.startsWith('{/*')
  )
}

export interface ColorViolation {
  file: string
  line: number
  value: string
}

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

/** 扫描 root 下所有源文件,返回颜色字面量违规清单(file 为相对 root 的 POSIX 路径) */
export function scanColors(root: string): ColorViolation[] {
  const violations: ColorViolation[] = []

  for (const file of walk(root)) {
    const rel = toPosix(relative(root, file))
    if (isAllowlisted(rel)) continue

    const lines = readFileSync(file, 'utf8').split('\n')
    lines.forEach((line, i) => {
      if (isCommentLine(line)) return

      const matches = line.match(HEX_RE)
      if (matches) {
        for (const value of matches) {
          violations.push({ file: rel, line: i + 1, value })
        }
      }
      // rgba/rgb 内联颜色检测(防止玻璃拟态/硬编码阴影回潮)
      if (RGB_RE.test(line)) {
        const m = line.match(/\brgba?\([^)]*\)/)
        violations.push({ file: rel, line: i + 1, value: m?.[0] ?? 'rgba(...)' })
      }
    })
  }

  return violations
}

function main(): void {
  // import.meta.dir 是 bun 专有 API,改用 fileURLToPath 保证 node(vitest)下可导入
  const root = join(dirname(fileURLToPath(import.meta.url)), '..', 'src')
  const violations = scanColors(root)

  if (violations.length > 0) {
    console.error('❌ 发现硬编码颜色,请使用 var(--color-*) semantic token:\n')
    for (const v of violations) {
      console.error(`  ${v.file}:${v.line}  ${v.value}`)
    }
    console.error(`\n共 ${violations.length} 处。`)
    process.exit(1)
  }

  console.log('✅ 无硬编码颜色(DOM 场景)或所有颜色字面量均在白名单内')
}

// vitest 进程(VITEST=true)仅导入模块做契约测试,不触发扫描;直接执行时才运行 main
if (!process.env.VITEST) {
  main()
}
