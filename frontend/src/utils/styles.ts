/**
 * 公共样式常量 — 统一组件的 Tailwind class 字符串
 * 避免重复内联，确保视觉一致性
 */

/** 幽灵按钮基础（无背景，hover 微亮） */
export const btnGhost =
  'flex items-center justify-center rounded transition-colors duration-150 hover:bg-surface-hover active:bg-surface-active'

/** 图标按钮（小尺寸正方形） */
export const btnIcon = `${btnGhost} w-6 h-6 text-text-tertiary hover:text-text-primary`

/** 输入框基础 */
export const inputBase =
  'bg-surface-elevated border border-border rounded-md px-2.5 py-1.5 text-[13px] text-text-primary placeholder:text-text-tertiary focus:border-accent/40 transition-colors duration-150'

/** 标题/分组标签 */
export const labelCaption =
  'text-[10px] font-semibold text-text-tertiary uppercase tracking-wider select-none'

/** 数据值（等宽） */
export const dataValue = 'text-[12px] font-mono font-medium text-text-primary'

/** 数据标签 */
export const dataLabel = 'text-[11px] text-text-tertiary'

/** 主操作按钮 */
export const btnPrimary =
  'bg-accent text-canvas font-semibold text-[12px] px-3 py-1.5 rounded-md transition-all duration-150 hover:brightness-110 active:scale-[0.97] active:-translate-y-[1px] shadow-glow'

/** 危险操作按钮 */
export const btnDanger =
  'bg-error/10 text-error font-semibold text-[12px] px-3 py-1.5 rounded-md transition-all duration-150 hover:bg-error/20 active:scale-[0.97]'
