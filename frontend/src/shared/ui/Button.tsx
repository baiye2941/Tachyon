import { mergeProps, type JSX, type Component } from 'solid-js'
import { Dynamic } from 'solid-js/web'

/**
 * Tachyon 统一按钮组件(Iteration 02)
 *
 * 消除全局 40+ 处内联 button 重复,统一视觉变体/尺寸/形状,
 * 强制 WCAG 2.5.8 触控目标(icon-sm 通过 CSS ::before 扩展到 44px),
 * 颜色自动跟随 Iteration 01 的语义 token。
 *
 * 使用:
 *   <Button variant="primary" onClick={...}>新建下载</Button>
 *   <Button variant="ghost" shape="icon-sm" aria-label="关闭"><XIcon /></Button>
 */

export type ButtonVariant = 'primary' | 'secondary' | 'ghost' | 'danger'
export type ButtonSize = 'sm' | 'md' | 'lg'
export type ButtonShape = 'default' | 'icon' | 'icon-sm'

const VARIANT_CLASS: Record<ButtonVariant, string> = {
  primary: 'btn-primary',
  secondary: 'btn-secondary',
  ghost: 'btn-ghost',
  danger: 'btn-danger',
}

const SIZE_CLASS: Record<ButtonSize, string> = {
  sm: 'btn-sm',
  md: 'btn-md',
  lg: 'btn-lg',
}

const SHAPE_CLASS: Record<ButtonShape, string> = {
  default: '',
  icon: 'icon-btn btn-icon',
  'icon-sm': 'icon-btn-sm btn-icon-sm',
}

export interface ButtonProps {
  variant?: ButtonVariant
  size?: ButtonSize
  shape?: ButtonShape
  disabled?: boolean
  loading?: boolean
  fullWidth?: boolean
  as?: string
  class?: string
  style?: JSX.CSSProperties
  'aria-label'?: string
  title?: string
  type?: 'button' | 'submit' | 'reset'
  children?: JSX.Element
  onClick?: (e: MouseEvent) => void
  onFocus?: (e: FocusEvent) => void
  onBlur?: (e: FocusEvent) => void
  onMouseEnter?: (e: MouseEvent) => void
  onMouseLeave?: (e: MouseEvent) => void
}

const Button: Component<ButtonProps> = (rawProps) => {
  const props = mergeProps(
    {
      variant: 'secondary' as ButtonVariant,
      size: 'md' as ButtonSize,
      shape: 'default' as ButtonShape,
      disabled: false,
      loading: false,
      fullWidth: false,
      as: 'button',
      type: 'button' as 'button' | 'submit' | 'reset',
    },
    rawProps,
  )

  const classes = () => {
    const parts: string[] = []
    if (props.shape === 'default') {
      parts.push(VARIANT_CLASS[props.variant]!)
      parts.push(SIZE_CLASS[props.size]!)
    } else {
      parts.push(SHAPE_CLASS[props.shape]!)
    }
    if (props.fullWidth) parts.push('w-full')
    if (props.class) parts.push(props.class)
    return parts.filter(Boolean).join(' ')
  }

  return (
    <Dynamic
      component={props.as}
      class={classes()}
      disabled={props.disabled || props.loading}
      aria-busy={props.loading || undefined}
      aria-label={props['aria-label']}
      title={props.title}
      type={props.as === 'button' ? props.type : undefined}
      onClick={(e: MouseEvent) => {
        if (props.disabled || props.loading) return
        props.onClick?.(e)
      }}
      onFocus={props.onFocus}
      onBlur={props.onBlur}
      onMouseEnter={props.onMouseEnter}
      onMouseLeave={props.onMouseLeave}
      style={props.style}
    >
      {props.children}
    </Dynamic>
  )
}

export default Button
