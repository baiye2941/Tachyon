import { describe, it, expect, beforeEach, vi, afterEach } from 'vitest'

let toastModule: typeof import('../toast')

describe('toast store', () => {
  beforeEach(async () => {
    vi.resetModules()
    vi.useFakeTimers()
    toastModule = await import('../toast')
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it('addToast 添加一条 toast 后 toasts() 长度变为 1', () => {
    toastModule.addToast('hello')
    expect(toastModule.toasts()).toHaveLength(1)
  })

  it('addToast 添加的 toast 包含正确的 message 和 type', () => {
    toastModule.addToast('操作成功', 'success')
    const toasts = toastModule.toasts()
    expect(toasts[0].message).toBe('操作成功')
    expect(toasts[0].type).toBe('success')
  })

  it('3000ms 后 toast 自动消失', () => {
    toastModule.addToast('自动消失')
    expect(toastModule.toasts()).toHaveLength(1)

    vi.advanceTimersByTime(3000)
    expect(toastModule.toasts()).toHaveLength(0)
  })

  it('removeToast 手动移除指定 toast', () => {
    toastModule.addToast('第一条')
    toastModule.addToast('第二条')
    expect(toastModule.toasts()).toHaveLength(2)

    const id = toastModule.toasts()[0].id
    toastModule.removeToast(id)
    expect(toastModule.toasts()).toHaveLength(1)
    expect(toastModule.toasts()[0].message).toBe('第二条')
  })
})
