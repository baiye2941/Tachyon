// confirm store — 应用内统一确认请求(Iteration 11)
//
// 背景:破坏性操作(删除)原本散落在 window.confirm(Invoke 包装层)与
// @tauri-apps/plugin-dialog(批量删除)两套原生 UI,视觉与品牌割裂,且批量
// 删除会级联弹出 N+1 个确认框。本 store 提供单一确认请求信号,App 根渲染
// 唯一 ConfirmDialog 实例,所有删除入口(DetailPanel/ContextMenu/批量)
// 统一走 requestConfirm → Promise<boolean>,确认 UI 控制权交还应用层。
//
// 后端 confirmation token 机制(requestConfirmation)不受影响,安全边界完整。
import { createSignal, untrack } from 'solid-js'

/** 确认按钮视觉调性 */
export type ConfirmTone = 'primary' | 'danger'

export interface ConfirmOptions {
  /** 是否显示“同时删除本地文件”复选项 */
  showDeleteLocalFileOption?: boolean
  /** 复选项文案 */
  deleteLocalFileLabel?: string
  /** 复选项说明 */
  deleteLocalFileDescription?: string
  /** 复选项默认值。删除本地文件是高风险动作,默认保持 false。 */
  deleteLocalFileDefault?: boolean
}

export interface ConfirmResult {
  /** 用户是否确认执行 */
  ok: boolean
  /** 用户是否选择同时删除本地文件 */
  deleteLocalFile: boolean
}

export interface ConfirmRequest extends ConfirmOptions {
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
  resolve: (result: ConfirmResult) => void
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
): Promise<ConfirmResult> {
  return new Promise<ConfirmResult>((resolve) => {
    // F-23:覆盖前先 resolve 旧的 pending 请求为 {ok:false}。
    // 旧实现直接 setPending 覆盖,旧 resolve 闭包被丢弃,旧 Promise 永久 pending(内存泄漏)。
    // 现在确保旧请求以"取消"语义收尾,避免泄漏并符合用户预期(被新请求取代即视为取消)。
    // untrack:仅读取当前 pending 快照用于清理,不建立响应式订阅(此处非跟踪作用域)。
    const prev = untrack(() => pending())
    if (prev) {
      setPending(null)
      prev.resolve({ ok: false, deleteLocalFile: false })
    }
    setPending({ ...opts, resolve })
  })
}

/**
 * 解决当前待决确认请求
 *
 * 由 App 根渲染的 ConfirmDialog 在用户点击确认/取消时调用。
 */
export function resolveConfirm(result: boolean | ConfirmResult): void {
  const req = pending()
  if (req) {
    setPending(null)
    req.resolve(typeof result === 'boolean' ? { ok: result, deleteLocalFile: false } : result)
  }
}

/** 当前待决确认请求(只读访问器,供 ConfirmDialog 绑定) */
export const $confirm = {
  pending,
}
