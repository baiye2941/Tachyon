import { For, createSignal, onCleanup, onMount } from 'solid-js'
import type { ToastMessage } from '../types'
import { XIcon } from './icons'
import Button from '../shared/ui/Button'
import { tr } from '../i18n'
import { useReducedMotion } from '../hooks/useReducedMotion'

const [toasts, setToasts] = createSignal<ToastMessage[]>([])

let _toastCounter = 0

export function addToast(toast: Omit<ToastMessage, 'id'>) {
  const id = `toast-${_toastCounter++}`
  const newToast: ToastMessage = { ...toast, id, duration: toast.duration ?? 5000 }
  setToasts(prev => [...prev.slice(-2), newToast])

  const timer = setTimeout(() => {
    removeToast(id)
  }, newToast.duration)

  return () => clearTimeout(timer)
}

export function dismissToast(id: string) {
  setToasts(prev => prev.map(t => t.id === id ? { ...t, closing: true } : t))
}

export function removeToast(id: string) {
  setToasts(prev => prev.filter(t => t.id !== id))
}

export function getToasts() {
  return toasts()
}

export default function ToastContainer() {
  return (
    <div
      class="fixed flex flex-col gap-2 pointer-events-none"
      role="status"
      aria-live="polite"
      style={{
        top: '48px',
        right: '16px',
        'z-index': 'var(--z-toast)',
        'max-width': '360px',
      }}
    >
      <For each={toasts()}>
        {(toast) => (
          <ToastItem toast={toast} />
        )}
      </For>
    </div>
  )
}

function ToastItem(props: { toast: ToastMessage }) {
  const reducedMotion = useReducedMotion()
  const [exiting, setExiting] = createSignal(false)
  let durationTimer: number | null = null
  let exitTimer: number | null = null

  const clearTimers = () => {
    if (durationTimer !== null) {
      clearTimeout(durationTimer)
      durationTimer = null
    }
    if (exitTimer !== null) {
      clearTimeout(exitTimer)
      exitTimer = null
    }
  }

  const startExit = () => {
    if (exiting()) return
    clearTimers()
    if (reducedMotion()) {
      removeToast(props.toast.id)
      return
    }
    setExiting(true)
    // eslint-disable-next-line solid/reactivity
    exitTimer = window.setTimeout(() => {
      exitTimer = null
      removeToast(props.toast.id)
    }, 200)
  }

  const startTimer = () => {
    const { duration } = props.toast
    // eslint-disable-next-line solid/reactivity
    durationTimer = window.setTimeout(() => {
      durationTimer = null
      startExit()
    }, duration)
  }

  onMount(() => {
    if (props.toast.closing) {
      startExit()
    } else {
      startTimer()
    }
  })
  onCleanup(() => clearTimers())

  const indicatorColor = () => {
    switch (props.toast.type) {
      case 'success': return 'var(--color-success)'
      case 'error': return 'var(--color-error)'
      case 'warning': return 'var(--color-warning)'
      case 'info': return 'var(--color-info)'
      default: return 'var(--color-success)'
    }
  }

  return (
    <div
      class="pointer-events-auto toast-item"
      classList={{ 'toast--exiting': exiting() }}
      style={{
        background: 'var(--color-bg-elevated)',
        border: '1px solid var(--color-border-default)',
        'border-radius': '12px',
        padding: '12px 16px',
        'box-shadow': 'var(--shadow-lg)',
        display: 'flex',
        gap: '12px',
        overflow: 'hidden',
      }}
      onMouseEnter={() => {
        if (!exiting()) clearTimers()
      }}
      onMouseLeave={() => {
        if (!exiting()) startTimer()
      }}
    >
      {/* Indicator */}
      <div
        style={{
          width: '3px',
          'border-radius': '2px',
          'flex-shrink': 0,
          background: indicatorColor(),
        }}
      />

      {/* Content */}
      <div class="flex-1 min-w-0">
        <div
          class="truncate"
          style={{
            'font-size': '14px',
            color: 'var(--color-text-title)',
            'font-weight': 500,
          }}
        >
          {props.toast.title}
        </div>
        {props.toast.description && (
          <div
            style={{
              'font-size': '12px',
              color: 'var(--color-text-secondary)',
              'margin-top': '2px',
            }}
          >
            {props.toast.description}
          </div>
        )}
        {props.toast.actions && props.toast.actions.length > 0 && (
          <div class="flex items-center gap-3" style={{ 'margin-top': '8px' }}>
            <For each={props.toast.actions}>
              {(action) => (
                <Button
                  variant="ghost"
                  size="sm"
                  aria-label={tr("toast.aria.closeToast")}
                  style={{ 'font-size': '12px', padding: '0 4px' }}
                  onClick={() => {
                    action.onClick()
                    startExit()
                  }}
                >
                  {action.label}
                </Button>
              )}
            </For>
          </div>
        )}
      </div>

      {/* Close */}
      <Button
        variant="ghost"
        shape="icon-sm"
        aria-label={tr("toast.aria.closeNotification")}
        onClick={() => startExit()}
      >
        <XIcon />
      </Button>
    </div>
  )
}
