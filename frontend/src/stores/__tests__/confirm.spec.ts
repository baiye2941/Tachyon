import { describe, it, expect, beforeEach, vi } from 'vitest'

// Iteration 11:confirm store 单测
// 验证 requestConfirm / resolveConfirm 的 Promise 语义与并发安全。

let confirmModule: typeof import('../confirm')

beforeEach(async () => {
  vi.resetModules()
  confirmModule = await import('../confirm')
  // 重置 pending 状态:resolve 任何可能残留的请求
  confirmModule.resolveConfirm(false)
})

describe('confirm store', () => {
  it('requestConfirm 返回未 resolve 的 Promise', () => {
    const p = confirmModule.requestConfirm({
      title: '测试',
      message: '消息',
    })
    expect(p).toBeInstanceOf(Promise)
    // 立即 resolve 为 false,避免悬挂 Promise 影响后续测试
    confirmModule.resolveConfirm(false)
    return expect(p).resolves.toBe(false)
  })

  it('requestConfirm 写入 pending 信号', () => {
    expect(confirmModule.$confirm.pending()).toBeNull()
    confirmModule.requestConfirm({
      title: '删除任务',
      message: '确定吗',
      confirmLabel: '删除',
      tone: 'danger',
    })
    const req = confirmModule.$confirm.pending()
    expect(req).not.toBeNull()
    expect(req?.title).toBe('删除任务')
    expect(req?.message).toBe('确定吗')
    expect(req?.confirmLabel).toBe('删除')
    expect(req?.tone).toBe('danger')
    confirmModule.resolveConfirm(false)
  })

  it('resolveConfirm(true) 使 Promise resolve 为 true', async () => {
    const p = confirmModule.requestConfirm({
      title: 't',
      message: 'm',
    })
    confirmModule.resolveConfirm(true)
    await expect(p).resolves.toBe(true)
  })

  it('resolveConfirm(false) 使 Promise resolve 为 false', async () => {
    const p = confirmModule.requestConfirm({
      title: 't',
      message: 'm',
    })
    confirmModule.resolveConfirm(false)
    await expect(p).resolves.toBe(false)
  })

  it('resolveConfirm 后 pending 清空', () => {
    confirmModule.requestConfirm({ title: 't', message: 'm' })
    expect(confirmModule.$confirm.pending()).not.toBeNull()
    confirmModule.resolveConfirm(true)
    expect(confirmModule.$confirm.pending()).toBeNull()
  })

  it('resolveConfirm 无 pending 时不报错', () => {
    expect(() => confirmModule.resolveConfirm(true)).not.toThrow()
  })

  it('tone 默认为 undefined(primary)', () => {
    confirmModule.requestConfirm({ title: 't', message: 'm' })
    expect(confirmModule.$confirm.pending()?.tone).toBeUndefined()
    confirmModule.resolveConfirm(false)
  })

  it('自定义 confirmLabel/cancelLabel 透传', () => {
    confirmModule.requestConfirm({
      title: 't',
      message: 'm',
      confirmLabel: 'Yes',
      cancelLabel: 'No',
    })
    const req = confirmModule.$confirm.pending()
    expect(req?.confirmLabel).toBe('Yes')
    expect(req?.cancelLabel).toBe('No')
    confirmModule.resolveConfirm(false)
  })
})
