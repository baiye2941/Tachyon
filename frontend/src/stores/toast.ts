// toast store — 兼容层
// 原型 ToastContainer 自带富结构 toast 状态(addToast({type,title,description,actions,duration}))，
// 是唯一真相源。本模块保留旧的简单签名 addToast(message, type)，
// 内部转发到 ToastContainer，使现有调用点(downloads/TaskList/SettingsPanel/Topbar)零改动。
import {
  addToast as addRichToast,
  getToasts as getRichToasts,
  removeToast as removeRichToast,
} from '../components/ToastContainer'

export type ToastType = 'info' | 'success' | 'error'

export interface Toast {
  id: string
  message: string
  msg: string
  type: ToastType
}

// 旧签名：addToast(message, type) —— 映射到富结构 toast 的 title 字段
export function addToast(message: string, type: ToastType = 'info') {
  // ToastContainer 支持 'success' | 'error' | 'warning' | 'info'，旧的三态是其子集
  addRichToast({ type, title: message, duration: 3000 })
}

export function removeToast(id: string) {
  removeRichToast(id)
}

export function toasts(): Toast[] {
  return getRichToasts().map(toast => ({
    id: toast.id,
    message: toast.title,
    msg: toast.title,
    type: toast.type === 'warning' ? 'info' : toast.type,
  }))
}
