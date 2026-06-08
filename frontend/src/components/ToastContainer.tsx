import { For, createSignal, onCleanup, onMount } from 'solid-js'
import type { ToastMessage } from '../types'
import { XIcon } from './icons'

const [toasts, setToasts] = createSignal<ToastMessage[]>([])

export function addToast(toast: Omit<ToastMessage, 'id'>) {
  const id = Math.random().toString(36).slice(2)
  const newToast: ToastMessage = { ...toast, id, duration: toast.duration ?? 5000 }
  setToasts(prev => [...prev.slice(-2), newToast])

  const timer = setTimeout(() => {
    removeToast(id)
  }, newToast.duration)

  return () => clearTimeout(timer)
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
      style={{
        top: '48px',
        right: '16px',
        'z-index': 100,
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
  let timer: number | null = null

  const startTimer = () => {
    const { id, duration } = props.toast
    timer = window.setTimeout(() => {
      removeToast(id)
    }, duration)
  }

  const clearTimer = () => {
    if (timer) clearTimeout(timer)
  }

  onMount(startTimer)
  onCleanup(() => clearTimer())

  const indicatorColor = () => {
    switch (props.toast.type) {
      case 'success': return '#00D4AA'
      case 'error': return '#EF4444'
      case 'warning': return '#F59E0B'
      case 'info': return '#00B4D8'
      default: return '#00D4AA'
    }
  }

  return (
    <div
      class="pointer-events-auto"
      style={{
        background: 'rgba(18, 18, 26, 0.95)',
        border: '1px solid rgba(255, 255, 255, 0.08)',
        'border-radius': '12px',
        padding: '12px 16px',
        'box-shadow': '0 8px 24px rgba(0, 0, 0, 0.5)',
        display: 'flex',
        gap: '12px',
        overflow: 'hidden',
        animation: 'toast-in 300ms cubic-bezier(0.32, 0.72, 0, 1)',
      }}
      onMouseEnter={() => {
        clearTimer()
      }}
      onMouseLeave={() => {
        startTimer()
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
            color: '#F0F0F5',
            'font-weight': 500,
          }}
        >
          {props.toast.title}
        </div>
        {props.toast.description && (
          <div
            style={{
              'font-size': '12px',
              color: '#A0A0B0',
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
                <button
                  class="hover-accent hover-underline"
                  style={{
                    'font-size': '12px',
                    color: '#00D4AA',
                    background: 'none',
                    border: 'none',
                    cursor: 'pointer',
                    padding: 0,
                  }}
                  onClick={() => {
                    action.onClick()
                    removeToast(props.toast.id)
                  }}
                >
                  {action.label}
                </button>
              )}
            </For>
          </div>
        )}
      </div>

      {/* Close */}
      <button
        class="icon-btn-sm"
        style={{
          width: '20px',
          height: '20px',
          color: '#6B7280',
          background: 'none',
          border: 'none',
          cursor: 'pointer',
        }}
        onClick={() => removeToast(props.toast.id)}
      >
        <XIcon />
      </button>
    </div>
  )
}
