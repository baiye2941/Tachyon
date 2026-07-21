//! 应用日志与 panic 兜底:release 崩溃可诊断地基
//!
//! ## 背景
//!
//! release 配置 `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`
//! 使应用无控制台窗口,此前 `tracing_subscriber::fmt()` 默认输出到 stderr 被系统丢弃,
//! 用户只见应用闪退/前端无响应,无法看到任何 panic 信息。叠加原 `panic = "abort"`(已
//! 改回 unwind),后台任务 panic 直接杀进程,日志又走 stderr 丢失,导致 release bug 极难诊断。
//!
//! ## 本模块职责
//!
//! 1. 引入 `tracing-appender` 滚动文件日志(`~/.tachyon/logs/app.YYYY-MM-DD.log`)
//! 2. panic hook **直写** `panic.log`(绕过非阻塞缓冲,保证 panic 落盘后再 unwind)
//! 3. 返回 `LogGuard` 持有非阻塞 appender 工作线程句柄,随应用生命周期存活;
//!    `panic = unwind` 模式下,正常退出或 panic unwind 时 `LogGuard::drop` 触发 flush。
//!
//! ## 可测试性
//!
//! - `log_dir()` 支持 `TACHYON_LOG_DIR` 环境变量覆盖,测试与用户均可重定向日志目录。
//! - panic 格式化与写盘逻辑抽为纯函数 `format_panic_line` / `write_panic_line`,
//!   便于单元测试,避免依赖全局 `OnceLock` 与真实 panic。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// panic.log 的绝对路径(panic hook 直写此处,绕过非阻塞缓冲)。
///
/// 用 `OnceLock` 而非直接 capture 到 hook 闭包,是因为 hook 是全局静态的,
/// 路径在 `init_logging` 时确定,panic 时从全局读取,避免闭包持有 `&PathBuf` 生命周期问题。
static PANIC_LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// 日志初始化返回的守卫。
///
/// 调用方必须保活至应用生命周期结束:`file_guard` 持有 `WorkerGuard`,
/// 其 `Drop` 会 flush 非阻塞文件 appender 的剩余缓冲并 join 工作线程。
/// `panic = unwind` 模式下,主流程正常返回或 panic unwind 时均触发 `Drop` flush。
pub struct LogGuard {
    /// 非阻塞文件 appender 的工作线程守卫。`None` 表示日志目录不可用,退化为 stderr-only。
    _file_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

/// 解析 Tachyon 日志目录。
///
/// 优先级:
/// 1. `TACHYON_LOG_DIR` 环境变量(测试/用户自定义)
/// 2. `<user_home>/.tachyon/logs`(与 store/config 同根,保持数据目录一致性)
///
/// 失败时回退当前目录 `./logs`。
fn log_dir() -> PathBuf {
    if let Some(custom) = std::env::var_os("TACHYON_LOG_DIR") {
        return PathBuf::from(custom);
    }
    tachyon_core::config::dirs()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tachyon")
        .join("logs")
}

/// 将 panic 信息格式化为单条日志行(含时间戳、位置、消息、backtrace)。
///
/// 抽为纯函数便于单元测试:不依赖全局状态,输入输出确定。
fn format_panic_line(location: &str, msg: &str, backtrace: &std::backtrace::Backtrace) -> String {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    format!("[{now}] PANIC at {location}: {msg}\n{backtrace}\n\n")
}

/// 将一条 panic 日志行同步追加写入指定文件。
///
/// 直写(绕过非阻塞缓冲)是 panic 落盘的关键:非阻塞 appender 的缓冲在 panic 路径下
/// 可能未被 flush 就 unwind。返回 `Ok(())` 仅表示写入成功,失败时调用方忽略(panic 路径
/// 不应再触发新 panic)。
fn write_panic_line(path: &Path, line: &str) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(line.as_bytes())?;
    f.flush()
}

/// 安装全局 panic hook。
///
/// 无论 tracing 是否已初始化都安装,保证即使日志层未就绪,panic 信息也能落盘到 `panic.log`。
/// hook 做两件事:
/// 1. 直写 `panic.log`(同步 `std::fs` append,绕过非阻塞缓冲,保证 panic 落盘)
/// 2. 通过 `tracing::error!` 记录(进非阻塞文件层,unwind flush 时落盘;tracing 未就绪时为 no-op)
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown location>".to_string());
        // 提取 panic 消息(payload 可能是 &str / String / 其他)
        let payload = info.payload();
        let msg = if let Some(s) = payload.downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".to_string()
        };
        // 捕获 backtrace(strip=true 下可能仅剩地址,但 panic.location 已给文件:行:列)
        let bt = std::backtrace::Backtrace::force_capture();
        let line = format_panic_line(&location, &msg, &bt);

        // 1. 直写 panic.log:绕过非阻塞缓冲,保证 panic 落盘后再 unwind
        if let Some(path) = PANIC_LOG_PATH.get() {
            let _ = write_panic_line(path, &line);
        }

        // 2. 通过 tracing 记录(进非阻塞文件层,unwind 时 flush 落盘)
        tracing::error!(
            target = "panic",
            panic.location = %location,
            "应用 panic: {msg}"
        );
    }));
}

/// 初始化日志与 panic hook。
///
/// 返回 `LogGuard`,调用方必须保活至应用生命周期结束。
///
/// 即使日志初始化失败(如目录不可用、subscriber 已被占用),也会安装 panic hook(写
/// `panic.log`),保证最低限度可诊断。
pub fn init_logging() -> LogGuard {
    let dir = log_dir();
    let panic_log_path = dir.join("panic.log");
    // 提前创建日志目录,失败时退化为 stderr-only(panic hook 仍尝试写,写失败静默)
    let dir_ok = std::fs::create_dir_all(&dir).is_ok();
    let _ = PANIC_LOG_PATH.set(panic_log_path);

    // 先装 panic hook:即使后续 tracing 初始化 panic 也能落盘
    install_panic_hook();

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let file_guard = if dir_ok {
        // 滚动文件 appender:按天滚动 app.YYYY-MM-DD.log
        let file_appender = tracing_appender::rolling::daily(&dir, "app.log");
        let (non_blocking, worker_guard) = tracing_appender::non_blocking(file_appender);

        // 文件层(无 ANSI 转义,便于直接阅读)+ stderr 层(debug 控制台可见;
        // release windows_subsystem 下 stderr 写入被系统丢弃,不影响文件层)
        let file_layer = fmt::layer()
            .with_writer(non_blocking)
            .with_ansi(false)
            .with_target(true);
        let stderr_layer = fmt::layer().with_writer(std::io::stderr);

        // try_init:测试或已设全局 subscriber 时返回 Err,此时退化为 stderr-only
        let init_result = tracing_subscriber::registry()
            .with(env_filter)
            .with(file_layer)
            .with(stderr_layer)
            .try_init();
        if init_result.is_err() {
            // 全局 subscriber 已存在(如测试环境):退化为 stderr-only,文件 guard 仍保活(无害)
            let _ = tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .with_writer(std::io::stderr)
                .try_init();
        }
        Some(worker_guard)
    } else {
        // 日志目录创建失败:stderr-only(panic hook 仍尝试直写 panic.log)
        let _ = tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_writer(std::io::stderr)
            .try_init();
        None
    };

    tracing::info!(log_dir = %dir.display(), "日志系统已初始化");
    LogGuard {
        _file_guard: file_guard,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 `log_dir` 默认解析到 `<home>/.tachyon/logs`
    #[test]
    fn test_log_dir_default_under_tachyon_root() {
        // 清除环境变量覆盖,测试默认路径
        // SAFETY: 测试单线程运行,环境变量操作无数据竞争
        unsafe {
            std::env::remove_var("TACHYON_LOG_DIR");
        }
        let dir = log_dir();
        assert!(dir.ends_with("logs"), "应以 logs 结尾: {}", dir.display());
        assert!(
            dir.parent()
                .map(|p| p.ends_with(".tachyon"))
                .unwrap_or(false),
            "父目录应为 .tachyon: {}",
            dir.display()
        );
    }

    /// 验证 `TACHYON_LOG_DIR` 环境变量可覆盖日志目录
    #[test]
    fn test_log_dir_respects_env_override() {
        let tmp = tempfile::tempdir().expect("创建临时目录失败");
        // SAFETY: 测试单线程运行,环境变量操作无数据竞争
        unsafe {
            std::env::set_var("TACHYON_LOG_DIR", tmp.path());
        }
        let dir = log_dir();
        // SAFETY: 同上
        unsafe {
            std::env::remove_var("TACHYON_LOG_DIR");
        }
        assert_eq!(dir, tmp.path(), "TACHYON_LOG_DIR 应覆盖默认日志目录");
    }

    /// 验证 `format_panic_line` 包含位置与消息
    #[test]
    fn test_format_panic_line_contains_location_and_msg() {
        let bt = std::backtrace::Backtrace::force_capture();
        let line = format_panic_line("foo.rs:42:7", "test boom", &bt);
        assert!(line.contains("PANIC"), "应含 PANIC 标记: {line}");
        assert!(line.contains("foo.rs:42:7"), "应含位置: {line}");
        assert!(line.contains("test boom"), "应含消息: {line}");
    }

    /// 验证 `write_panic_line` 创建并追加到文件
    #[test]
    fn test_write_panic_line_creates_and_appends() {
        let tmp = tempfile::tempdir().expect("创建临时目录失败");
        let path = tmp.path().join("panic.log");

        write_panic_line(&path, "first panic\n").expect("首次写入失败");
        write_panic_line(&path, "second panic\n").expect("追加写入失败");

        let content = std::fs::read_to_string(&path).expect("读取失败");
        assert!(content.contains("first panic"), "应含首次: {content}");
        assert!(content.contains("second panic"), "应含追加: {content}");
    }

    /// 验证 `init_logging` 不 panic 且返回 guard(全局 subscriber 可能已被占用,需容忍)
    #[test]
    fn test_init_logging_returns_guard_without_panic() {
        // 全局 subscriber/panic hook 在同一进程内可能已被其他测试初始化,
        // init_logging 内部用 try_init 容忍,此处仅验证不 panic
        let _guard = init_logging();
    }
}
