//! 后台任务 panic 隔离:防止单个 spawn 任务 panic 杀全应用
//!
//! ## 背景
//!
//! AGENTS.md 教训 "NEVER 让协议层产出的 ByteStream 无超时保护" 等聚焦业务并发,但
//! 漏了一条更基础的地基:**后台 spawn 任务的 panic 在 `panic = abort` 下会杀整个进程**。
//! 本项目已把 profile 改回 `unwind`,但这只让 catch_unwind 可工作——还必须**在每个
//! 长期运行的 spawn 任务外包 catch_unwind**,才能把单个下载/进度聚合/chunk reader 的
//! panic 隔离在该任务内,不影响其他任务与 UI。
//!
//! ## 本模块职责
//!
//! `spawn_isolated` 包裹一个 future:
//! 1. 用 `AssertUnwindSafe` + `catch_unwind` 捕获 panic
//! 2. panic 时经 tracing 记录(进文件层,release 可见),任务标记失败
//! 3. 不重新 panic,让 JoinHandle 正常完成(调用方拿不到 panic Err,但进程存活)
//!
//! 这是对 `tokio::spawn` 的薄封装,语义保持一致,仅增加 panic 隔离。
//!
//! ## 适用范围
//!
//! 仅用于"长期运行的后台任务"——下载 task_fn、进度聚合、chunk reader worker、剪贴板
//! 监听。**不**用于短命一次性 spawn(如 hub verify 的 JoinSet),那些用 JoinSet 的
//! `await` 已能感知 panic。
//!
//! ## 与 panic 策略的对齐
//!
//! 依赖 `panic = "unwind"`(已在根 Cargo.toml release profile 改回)。
//! 若未来切回 abort,本模块 catch_unwind 失效(abort 绕过 unwind),需改用进程隔离。

use futures::FutureExt;
use std::future::Future;
use std::panic::AssertUnwindSafe;

/// 包裹一个 future 并 spawn 到 runtime,捕获其 panic 使其不传播到进程级。
///
/// 返回 `JoinHandle<()>`(任务 panic 时以 `Ok(())` 完成,而非 `Err(JoinError)`)。
/// panic 信息经 tracing 记录到日志文件层,release 下可见。
///
/// # 设计权衡
///
/// - **不返回 panic Err**:调用方(如 `DownloadSupervisor`)已有自己的错误状态机,
///   task_fn 的业务错误经 progress broker 上报;此处仅兜底"未预期 panic",不让
///   JoinHandle 因 panic 变成 Err(否则 `wait_for_handle` 的 `Ok(result)` 分支会
///   误判任务"正常完成")。
/// - **tracing 记录位置**:panic 发生在 spawn 任务的栈上,backtrace 指向任务内部,
///   配合 `panic.log` 的文件:行:列,足以定位。
pub fn spawn_isolated<F>(name: &'static str, fut: F) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    spawn_isolated_with_panic_hook(name, fut, |_| {})
}

/// 同 `spawn_isolated`,但允许调用方在 panic 被捕获后执行自定义收尾逻辑。
///
/// `on_panic` 在 panic 被捕获后(任务 future 已展开)、JoinHandle 完成前同步调用,
/// 接收 panic 消息字符串(拥有所有权,便于在内部 `tokio::spawn` 中跨 await 使用)。
/// 典型用途:`task_fn` panic 后调用 `mark_task_failed_and_cleanup`
/// 把任务转 Failed 态并清理 runtime,避免栈展开跳过终态清理导致任务卡死 Downloading。
///
/// `on_panic` 若需执行 async 操作(如清理 runtime),应在内部 `tokio::spawn` 一个
/// 新任务(因为当前已在 panic 捕获点,无法直接 await)。
pub fn spawn_isolated_with_panic_hook<F, H>(
    name: &'static str,
    fut: F,
    on_panic: H,
) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
    H: FnOnce(String) + Send + 'static,
{
    tokio::spawn(async move {
        let result = AssertUnwindSafe(fut).catch_unwind().await;
        if let Err(panic_payload) = result {
            // panic_payload 是 Box<dyn Any + Send>,提取消息同 panic hook 逻辑
            let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "<non-string panic payload>".to_string()
            };
            tracing::error!(
                target: "spawn_isolated",
                task = name,
                panic.msg = %msg,
                "后台任务 panic 已隔离,应用继续运行"
            );
            // 调用方收尾(如标记任务 Failed + 清理 runtime)
            on_panic(msg);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 `spawn_isolated` 捕获 panic,JoinHandle 正常完成而非 panic
    #[tokio::test]
    async fn test_spawn_isolated_catches_panic() {
        let handle = spawn_isolated("test-panic", async {
            panic!("test boom in isolated task");
        });
        // JoinHandle 应正常完成(不返回 JoinError)
        let result = handle.await;
        assert!(
            result.is_ok(),
            "spawn_isolated 应捕获 panic,JoinHandle 正常完成,实际: {result:?}"
        );
    }

    /// 验证正常完成的任务不受影响
    #[tokio::test]
    async fn test_spawn_isolated_passes_through_normal_completion() {
        let handle = spawn_isolated("test-ok", async {
            // 正常完成,无 panic
        });
        let result = handle.await;
        assert!(result.is_ok(), "正常任务应 Ok 完成: {result:?}");
    }

    /// 验证 panic 后 runtime 仍可用(进程未 abort)
    #[tokio::test]
    async fn test_runtime_survives_isolated_panic() {
        let h1 = spawn_isolated("test-survive-panic", async {
            panic!("first panic");
        });
        let _ = h1.await;
        // panic 后 spawn 第二个任务,验证 runtime 仍存活
        let h2 = spawn_isolated("test-survive-ok", async {});
        let r2 = h2.await;
        assert!(r2.is_ok(), "panic 后 runtime 应仍能 spawn 与 await: {r2:?}");
    }

    /// 验证多任务并发,其中一个 panic 不影响其他
    #[tokio::test]
    async fn test_concurrent_panic_does_not_affect_others() {
        let h1 = spawn_isolated("test-conc-panic", async {
            panic!("concurrent boom");
        });
        let h2 = spawn_isolated("test-conc-ok", async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        });
        let r1 = h1.await;
        let r2 = h2.await;
        assert!(r1.is_ok(), "panic 任务应被隔离: {r1:?}");
        assert!(r2.is_ok(), "正常任务不受影响: {r2:?}");
    }

    /// 验证 `spawn_isolated_with_panic_hook` 在 panic 时调用 on_panic 收尾回调
    #[tokio::test]
    async fn test_spawn_with_panic_hook_invokes_callback() {
        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let captured_clone = captured.clone();
        let handle = spawn_isolated_with_panic_hook(
            "test-hook-panic",
            async {
                panic!("hook boom");
            },
            move |msg: String| {
                *captured_clone.lock().unwrap() = Some(msg);
            },
        );
        let result = handle.await;
        assert!(result.is_ok(), "panic 应被隔离: {result:?}");
        let captured = captured.lock().unwrap().clone();
        assert_eq!(
            captured.as_deref(),
            Some("hook boom"),
            "on_panic 回调应收到 panic 消息,实际: {captured:?}"
        );
    }

    /// 验证正常完成时不调用 on_panic
    #[tokio::test]
    async fn test_spawn_with_panic_hook_not_invoked_on_normal_completion() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();
        let handle = spawn_isolated_with_panic_hook("test-hook-ok", async {}, move |_: String| {
            called_clone.store(true, Ordering::SeqCst);
        });
        let result = handle.await;
        assert!(result.is_ok());
        assert!(!called.load(Ordering::SeqCst), "正常完成不应调用 on_panic");
    }
}
