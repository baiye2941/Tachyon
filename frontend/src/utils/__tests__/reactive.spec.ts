import { describe, it, expect } from 'vitest'
import { createSignal } from 'solid-js'
import { createRootMemo, disposeAllRootMemos } from '../reactive'

describe('createRootMemo / disposeAllRootMemos (P3-10)', () => {
  it('createRootMemo 返回可响应依赖变化的 memo', () => {
    const [count, setCount] = createSignal(0)
    const doubled = createRootMemo(() => count() * 2)

    expect(doubled()).toBe(0)
    setCount(5)
    expect(doubled()).toBe(10)
    setCount(21)
    expect(doubled()).toBe(42)
  })

  it('disposeAllRootMemos 后 memo 不再响应依赖变化', () => {
    const [count, setCount] = createSignal(1)
    const squared = createRootMemo(() => count() * count())

    expect(squared()).toBe(1)

    // dispose 后 memo 的反应式计算图应被销毁
    disposeAllRootMemos()

    // 更新信号源:被 dispose 的 memo 不应再重新计算
    setCount(10)
    expect(squared()).toBe(1)
  })

  it('disposeAllRootMemos 可被安全地重复调用', () => {
    const [a] = createSignal('static')
    createRootMemo(() => a())

    // 多次调用不应抛错(重复 dispose 是幂等的)
    expect(() => {
      disposeAllRootMemos()
      disposeAllRootMemos()
    }).not.toThrow()

    // dispose 后仍可创建新 memo(注册表已清空,不残留)
    const [n, setN] = createSignal(2)
    const tripled = createRootMemo(() => n() * 3)
    expect(tripled()).toBe(6)
    setN(4)
    expect(tripled()).toBe(12)
  })

  it('多个 createRootMemo 互相独立 dispose', () => {
    const [a, setA] = createSignal(1)
    const [b, setB] = createSignal(100)
    const memoA = createRootMemo(() => a() + 1)
    const memoB = createRootMemo(() => b() - 1)

    expect(memoA()).toBe(2)
    expect(memoB()).toBe(99)

    // dispose 后两者都应停止响应
    disposeAllRootMemos()
    setA(50)
    setB(500)
    expect(memoA()).toBe(2)
    expect(memoB()).toBe(99)
  })
})
