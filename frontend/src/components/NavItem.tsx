import { Show } from 'solid-js'
import { Icon } from '../utils/icons'

interface NavItemProps {
  iconPath?: string
  iconName?: string
  label: string
  isActive: boolean
  onClick: () => void
  isExpanded: boolean
  count?: number | string
}

export default function NavItem(props: NavItemProps) {
  function handleKeyDown(e: KeyboardEvent) {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault()
      props.onClick()
    }
  }

  return (
    <button
      class={`relative w-full flex items-center gap-2.5 px-3 py-1.5 text-[12px] transition-all duration-150 ${
        props.isActive
          ? 'text-accent bg-accent-muted'
          : 'text-text-secondary hover:bg-white/[0.04] hover:text-text-primary hover:translate-x-[1px]'
      }`}
      onClick={() => props.onClick()}
      onKeyDown={handleKeyDown}
      aria-pressed={props.isActive}
    >
      <Show when={props.isActive}>
        <div class="absolute left-0 top-1/2 -translate-y-1/2 w-[2px] h-4 bg-accent rounded-full" />
      </Show>
      <Show
        when={props.iconName}
        fallback={
          <svg viewBox="0 0 24 24" class="w-4 h-4 shrink-0" fill="none" stroke="currentColor" stroke-width="1.5">
            <path d={props.iconPath} />
          </svg>
        }
      >
        <Icon name={props.iconName!} class="w-4 h-4 shrink-0" />
      </Show>
      <Show when={props.isExpanded}>
        <span class="flex-1 text-left whitespace-nowrap">{props.label}</span>
        <Show when={props.count !== undefined}>
          <span class="text-[10px] font-mono text-text-tertiary">{props.count}</span>
        </Show>
      </Show>
    </button>
  )
}
