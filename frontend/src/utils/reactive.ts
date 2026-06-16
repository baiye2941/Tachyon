import { createMemo, createRoot, type Accessor } from "solid-js";

/**
 * 模块级 root memo 的 dispose 注册表。
 *
 * 模块顶层 createMemo 创建的 computation 没有 Owner,
 * 无法被 SolidJS 自动 GC 回收。createRootMemo 把它们放入
 * createRoot 作用域,但 createRoot 返回的 dispose 闭包必须
 * 被显式调用才能释放。本注册表收集所有 dispose,供 HMR
 * 热替换时统一清理,避免反应式图泄漏。
 */
const rootMemoDisposers = new Set<() => void>();

/**
 * 在 createRoot 下创建派生 memo,并把 dispose 闭包注册到全局注册表。
 *
 * - 正常运行时:dispose 永不调用,memo 与应用同生命周期,无副作用。
 * - HMR 热替换时:main.tsx 注册的 `import.meta.hot.dispose` 会调用
 *   `disposeAllRootMemos()`,清理旧模块创建的 memo 计算图,
 *   避免热重载累积无法回收的反应式节点。
 *
 * 用法: const myMemo = createRootMemo(() => derivedValue())
 */
export function createRootMemo<T>(fn: () => T): Accessor<T> {
  let memo: Accessor<T> | undefined;
  const dispose = createRoot((disposeFn) => {
    // eslint-disable-next-line solid/reactivity -- memo 仅在 createRoot 初始化时赋值
    memo = createMemo(fn);
    return disposeFn;
  });
  rootMemoDisposers.add(dispose);
  return memo!;
}

/**
 * 释放所有通过 createRootMemo 创建的反应式计算图。
 *
 * 应在 HMR `import.meta.hot.dispose` 钩子中调用。
 * 非生产环境(import.meta.hot 不存在)下无需调用,
 * 因为应用关闭时整个进程退出,内存自然回收。
 */
export function disposeAllRootMemos(): void {
  rootMemoDisposers.forEach((dispose) => {
    try {
      dispose();
    } catch {
      // dispose 可能因 Owner 已失效而抛错,忽略以保证其余清理继续
    }
  });
  rootMemoDisposers.clear();
}
