interface ToggleProps {
  checked: boolean
  ariaLabel: string
  onChange?: (value: boolean) => void
}

export default function Toggle(props: ToggleProps) {
  return (
    <div
      class={`relative w-9 h-5 rounded-full cursor-pointer transition-colors duration-150 ${props.checked ? 'bg-accent' : 'bg-surface-elevated'}`}
      role="switch"
      aria-checked={props.checked}
      aria-label={props.ariaLabel}
      tabIndex={0}
      onClick={() => props.onChange?.(!props.checked)}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault()
          props.onChange?.(!props.checked)
        }
      }}
    >
      <div class={`absolute top-0.5 left-0.5 w-4 h-4 rounded-full bg-white transition-transform duration-150 ${props.checked ? 'translate-x-4' : ''}`} />
    </div>
  )
}
