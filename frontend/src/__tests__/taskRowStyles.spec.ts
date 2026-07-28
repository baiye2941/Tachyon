import { describe, it, expect } from 'vitest'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

// 任务行样式域回归测试:
// task-list.css 与 task-item.css 的 .task-row 系列规则曾近逐行重复,事实源分裂;
// TaskList.tsx 切换的 .task-list--dragging/.task-row--drop-target 曾无任何样式规则;
// 另有一批 tsx 零引用的死类。本测试锁定收敛结果:
// .task-row 系列单文件收敛于 task-item.css,拖拽态样式存在,死类彻底清除。
// vitest 的 cwd = frontend,用 node fs 读原始 CSS 文本断言。
const readCss = (name: string) =>
  readFileSync(join(process.cwd(), 'src/styles/components', name), 'utf8')

describe('任务行样式域收敛', () => {
  it('task-list.css 不再持有任何 .task-row 系列规则', () => {
    expect(readCss('task-list.css')).not.toMatch(/\.task-row[ {:-]/)
  })

  it('task-item.css 是 .task-row 系列的唯一事实源', () => {
    expect(readCss('task-item.css')).includes('.task-row')
  })

  it('合并后 .task-row 基础块保留 task-list.css 独有的 flex 布局声明', () => {
    const css = readCss('task-item.css')
    expect(css).toMatch(/\.task-row \{[^}]*display: flex/)
    expect(css).toMatch(/\.task-row \{[^}]*align-items: center/)
  })

  it('.task-row--drop-target 拖放目标样式存在', () => {
    expect(readCss('task-item.css')).includes('.task-row--drop-target')
  })

  it('.task-list--dragging 拖拽中样式存在(允许后代形式)', () => {
    expect(readCss('task-item.css')).includes('.task-list--dragging')
  })
})

describe('死类清除(tsx 零引用,substring 断言覆盖 BEM 派生)', () => {
  it.each(['.task-col-progress', '.task-col-speed', '.task-col-status'])(
    'task-item.css 不含死类 %s',
    (cls) => {
      expect(readCss('task-item.css')).not.includes(cls)
    },
  )

  it.each([
    '.detail-stat-grid',
    '.detail-stat-cell',
    '.detail-stat-label',
    '.detail-stat-value',
    '.bandwidth-sparkline',
    '.detail-action-delete',
  ])('detail-panel.css 不含死类 %s', (cls) => {
    expect(readCss('detail-panel.css')).not.includes(cls)
  })
})
