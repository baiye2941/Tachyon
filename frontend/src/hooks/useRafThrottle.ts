import { createEffect, onCleanup } from 'solid-js'

export interface RafThrottleOptions<T> {
  /** 数据源(支持 getter) */
  source: T | (() => T)
  /** 回调,在下一帧执行 */
  callback: (value: T) => void
  /** 是否启用,默认 true */
  enabled?: boolean | (() => boolean)
}

function resolve<T>(value: T | (() => T)): T {
  return typeof value === 'function' ? (value as () => T)() : value
}

/**
 * rAF 节流 hook(Iteration 09)。
 *
 * 将高频数据变化批量到下一帧执行,避免在同一帧内多次触发回调。
 * 适用于进度/速度等高频 signal 驱动的 UI 更新。
 *
 * @example
 * useRafThrottle({
 *   source: () => $totalSpeed.get(),
 *   callback: (speed) => speedHistory.pushSpeed(speed),
 * })
 */
export function useRafThrottle<T>(options: RafThrottleOptions<T>) {
  let rafId: number | null = null
  let latestValue: T | undefined
  let hasPending = false

  createEffect(() => {
    const enabled = resolve(options.enabled ?? true)
    if (!enabled) return

    const value = resolve(options.source)
    latestValue = value
    hasPending = true

    if (rafId === null) {
      rafId = requestAnimationFrame(() => {
        rafId = null
        if (hasPending) {
          options.callback(latestValue as T)
          hasPending = false
        }
      })
    }
  })

  onCleanup(() => {
    if (rafId !== null) {
      cancelAnimationFrame(rafId)
      rafId = null
    }
  })
}
