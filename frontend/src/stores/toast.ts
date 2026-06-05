import { createSignal } from 'solid-js'

export interface Toast {
  id: number
  message: string
  type: 'info' | 'success' | 'error'
}

let toastId = 0
const [toasts, setToasts] = createSignal<Toast[]>([])

export function addToast(message: string, type: Toast['type'] = 'info') {
  const id = ++toastId
  setToasts(prev => [...prev, { id, message, type }])
  setTimeout(() => {
    setToasts(prev => prev.filter(t => t.id !== id))
  }, 3000)
}

export function removeToast(id: number) {
  setToasts(prev => prev.filter(t => t.id !== id))
}

export { toasts }
