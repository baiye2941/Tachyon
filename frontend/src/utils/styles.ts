/**
 * 公共样式常量 — 统一组件的 class 字符串
 *
 * Iteration 02 修复:原版本引用了一整套不存在的 Tailwind 语义类
 * (bg-surface-hover / text-text-secondary / glass-panel 等),这些类
 * 在 Tailwind v4 + 当前 @theme 配置下不会生成任何 CSS,导致引用方
 * (BatchToolbar / CommandPalette)渲染为裸样式。
 *
 * 现改为代理到 index.css 中真实定义的 class,确保样式生效。
 * 新代码应直接使用 <Button> 组件而非这些常量。
 */

/** 幽灵按钮基础(无背景,hover 微亮)— 代理到 .btn-ghost */
export const btnGhost = 'btn-ghost'

/** 图标按钮(小尺寸 28×28 + 44px 触控扩展)— 代理到 .icon-btn-sm */
export const btnIcon = 'icon-btn-sm'

/** 输入框基础 — 代理到 .input(Iteration 01 已定义) */
export const inputBase = 'input'

/** 标题/分组标签 */
export const labelCaption =
  'text-[10px] font-semibold uppercase tracking-wider select-none'

/** 数据值(等宽) */
export const dataValue = 'mono text-[12px] font-medium'

/** 数据标签 */
export const dataLabel = 'text-[11px]'

/** 主操作按钮 — 代理到 .btn-primary */
export const btnPrimary = 'btn-primary btn-md'

/** 危险操作按钮 — 代理到 .btn-danger */
export const btnDanger = 'btn-danger btn-md'
