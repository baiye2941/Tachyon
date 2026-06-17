// confirm store — 应用内统一确认请求(Iteration 11)
//
// 背景:破坏性操作(删除)原本散落在 window.confirm(Invoke 包装层)与
// @tauri-apps/plugin-dialog(批量删除)两套原生 UI,视觉与品牌割裂,且批量
// 删除会级联弹出 N+1 个确认框。本 store 提供单一确认请求信号,App 根渲染
// 唯一 ConfirmDialog 实例,所有删除入口(DetailPanel/ContextMenu/批量)
// 统一走 requestConfirm → Promise<boolean>,确认 UI 控制权交还应用层。
//
// 后端 confirmation token 机制(requestConfirmation)不受影响,安全边界完整。
import { createSignal } from 'solid-js'

/** 确认按钮视觉调性 */
export type ConfirmTone = 'primary' | 'danger'

export interface ConfirmRequest {
  /** 对话框标题 */
  title: string
  /** 对话框描述信息 */
  message: string
  /** 确认按钮文本,默认「确认」 */
  confirmLabel?: string
  /** 取消按钮文本,默认「取消」 */
  cancelLabel?: string
  /** 确认按钮调性:danger 用于删除等破坏性操作(红色按钮) */
  tone?: ConfirmTone
  /** 确认回调,由 ConfirmDialog 在用户点击后调用 */
  resolve: (ok: boolean) => void
}

const [pending, setPending] = createSignal<ConfirmRequest | null>(null)

/**
 * 发起一次应用内确认请求,返回 Promise<boolean>
 *
 * - true:用户点击确认
 * - false:用户点击取消或关闭
 *
 * 同一时刻只允许一个待决确认请求;若上一个尚未 resolve,新请求会等待。
 */
export function requestConfirm(
  opts: Omit<ConfirmRequest, 'resolve'>,
): Promise<boolean> {
  return new Promise<boolean>((resolve) => {
    setPending({ ...opts, resolve })
  })
}

/**
 * 解决当前待决确认请求
 *
 * 由 App 根渲染的 ConfirmDialog 在用户点击确认/取消时调用。
 */
export function resolveConfirm(ok: boolean): void {
  const req = pending()
  if (req) {
    setPending(null)
    req.resolve(ok)
  }
}

/** 当前待决确认请求(只读访问器,供 ConfirmDialog 绑定) */
export const $confirm = {
  pending,
}
