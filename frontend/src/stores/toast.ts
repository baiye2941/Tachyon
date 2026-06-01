import { createStore } from 'solid-js/store'

export interface Toast {
  id: number
  message: string
  type: 'error' | 'success' | 'info'
}

let nextId = 0
const [toasts, setToasts] = createStore<Toast[]>([])

export function addToast(message: string, type: Toast['type'] = 'error', duration = 4000) {
  const id = nextId++
  setToasts([...toasts, { id, message, type }])
  setTimeout(() => {
    setToasts(toasts.filter(t => t.id !== id))
  }, duration)
}

export { toasts }
