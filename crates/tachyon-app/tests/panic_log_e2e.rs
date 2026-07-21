//! panic.log 端到端验证(release 崩溃可诊断地基的实证测试)
//!
//! 验证 Phase A 地基:release 下任何 panic 都会直写 `panic.log` 落盘,
//! 使"用户只见闪退"变为"panic.log 留下诊断证据"。
//!
//! 测试策略:
//! 1. 用 `TACHYON_LOG_DIR` 把日志目录重定向到临时目录(隔离,不污染用户 ~/.tachyon)
//! 2. 调 `init_logging()` 安装 panic hook + 设置 PANIC_LOG_PATH
//! 3. `catch_unwind` 触发一个 panic(hook 在 unwind 前执行,直写 panic.log)
//! 4. 读临时目录下 panic.log,验证包含 panic 消息与位置

use std::panic::{AssertUnwindSafe, catch_unwind};

/// 验证 panic 时 panic.log 被创建且包含 panic 消息
///
/// 这是 release-only bug 修复地基的核心实证:此前 panic 走 stderr,
/// release windows_subsystem 下 stderr 被丢弃,用户只见闪退;
/// 现在 panic hook 直写 panic.log(绕过非阻塞缓冲),保证落盘。
#[test]
fn test_panic_log_is_written_on_panic() {
    let tmp = tempfile::tempdir().expect("创建临时目录失败");
    // SAFETY: 测试单线程运行,TACHYON_LOG_DIR 仅本测试进程可见
    unsafe {
        std::env::set_var("TACHYON_LOG_DIR", tmp.path());
    }

    // 初始化日志:安装 panic hook + 设置 PANIC_LOG_PATH = <tmp>/panic.log
    let _guard = tachyon_app_lib::logging::init_logging();

    let panic_msg = "test release panic for e2e verification";
    // catch_unwind 触发 panic:panic hook 先于 unwind 执行,直写 panic.log
    let result = catch_unwind(AssertUnwindSafe(|| {
        panic!("{panic_msg}");
    }));
    assert!(result.is_err(), "catch_unwind 应捕获到 panic");

    // 验证 panic.log 被创建
    let panic_log_path = tmp.path().join("panic.log");
    assert!(
        panic_log_path.exists(),
        "panic.log 应被创建于 {}",
        panic_log_path.display()
    );

    // 验证 panic.log 包含 panic 消息
    let content = std::fs::read_to_string(&panic_log_path).expect("读取 panic.log 失败");
    assert!(
        content.contains(panic_msg),
        "panic.log 应包含 panic 消息 {panic_msg:?},实际内容:\n{content}"
    );

    // 验证 panic.log 包含位置标记(PANIC at ...rs:行:列)
    assert!(
        content.contains("PANIC at"),
        "panic.log 应包含 'PANIC at' 位置标记,实际:\n{content}"
    );
    assert!(
        content.contains("panic_log_e2e.rs"),
        "panic.log 应包含触发文件名 panic_log_e2e.rs,实际:\n{content}"
    );

    // 清理环境变量(仅本进程)
    // SAFETY: 测试单线程运行
    unsafe {
        std::env::remove_var("TACHYON_LOG_DIR");
    }
}

/// 验证正常流程(无 panic)不创建 panic.log
#[test]
fn test_panic_log_not_created_without_panic() {
    let tmp = tempfile::tempdir().expect("创建临时目录失败");
    // SAFETY: 测试单线程运行
    unsafe {
        std::env::set_var("TACHYON_LOG_DIR", tmp.path());
    }

    let _guard = tachyon_app_lib::logging::init_logging();

    // 不触发任何 panic
    let panic_log_path = tmp.path().join("panic.log");
    assert!(!panic_log_path.exists(), "无 panic 时不应创建 panic.log");

    // SAFETY: 测试单线程运行
    unsafe {
        std::env::remove_var("TACHYON_LOG_DIR");
    }
}

/// 验证连续多次 panic 都追加到同一个 panic.log(非覆盖)
#[test]
fn test_multiple_panics_append_to_same_panic_log() {
    let tmp = tempfile::tempdir().expect("创建临时目录失败");
    // SAFETY: 测试单线程运行
    unsafe {
        std::env::set_var("TACHYON_LOG_DIR", tmp.path());
    }

    let _guard = tachyon_app_lib::logging::init_logging();

    for i in 0..3 {
        let msg = format!("e2e append panic #{i}");
        let msg_for_closure = msg.clone();
        let _ = catch_unwind(AssertUnwindSafe(|| {
            panic!("{msg_for_closure}");
        }));
    }

    let panic_log_path = tmp.path().join("panic.log");
    let content = std::fs::read_to_string(&panic_log_path).expect("读取 panic.log 失败");
    for i in 0..3 {
        let msg = format!("e2e append panic #{i}");
        assert!(
            content.contains(&msg),
            "panic.log 应包含第 {i} 次 panic 消息 {msg:?}"
        );
    }

    // SAFETY: 测试单线程运行
    unsafe {
        std::env::remove_var("TACHYON_LOG_DIR");
    }
}
