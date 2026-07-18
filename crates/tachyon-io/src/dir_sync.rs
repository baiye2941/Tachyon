//! F-15(父目录 sync)实现 + 测试模块
//!
//! 审计 F-15:4 个 io 后端 close 仅 fsync 文件本身,不 sync 父目录。
//! Unix 断电后文件数据落盘但目录项创建未持久化,文件可能消失。多文件
//! torrent 需逐层新目录持久化。
//!
//! 行为:
//! 1. `tachyon-io` 导出 `pub fn sync_parent_dir(path: &Path) -> std::io::Result<()>`
//! 2. 4 个后端 `close()` 末尾调用 `sync_parent_dir(&self.path)`
//! 3. Unix:fsync 父目录;Windows:验证可打开(NTFS 日志保证)

use std::path::Path;

/// F-15: 持久化父目录项。
///
/// Unix:打开父目录并 `fsync` 落盘目录项创建/更新(断电后文件名仍可见)。
/// Windows:以 `FILE_FLAG_BACKUP_SEMANTICS` 打开父目录验证可访问
/// (NTFS rename 原子 + 日志保证目录项持久,无需 fsync)。
///
/// 根路径(`parent()` 为 None 或空)直接返回 Ok,不 panic。
pub fn sync_parent_dir(path: &Path) -> std::io::Result<()> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => return Ok(()),
    };
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_BACKUP_SEMANTICS(0x0200_0000):允许打开目录获得句柄;
        // share_mode READ|WRITE|DELETE(0x07):与其他句柄共存,避免 sharing violation。
        opts.custom_flags(0x0200_0000).share_mode(0x07);
    }
    let dir_file = opts.open(parent)?;
    #[cfg(not(target_os = "windows"))]
    {
        dir_file.sync_all()?;
    }
    #[cfg(target_os = "windows")]
    {
        let _ = dir_file;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use tempfile::NamedTempFile;

    // 引用待实现的函数:此时应触发 E0425(函数尚不存在)。
    // 实现契约:见模块末尾。
    use crate::sync_parent_dir;

    /// 验证:对已存在的临时文件调用 `sync_parent_dir(file.path())` 应返回 Ok。
    ///
    /// 行为:
    /// 1. 创建 NamedTempFile(其父目录为系统 tempdir)
    /// 2. 调用 `sync_parent_dir(tmp.path())`
    /// 3. 期望 Ok(())(父目录存在且可打开,Unix 下 fsync 成功,Windows 下仅验证可打开)
    #[test]
    fn test_sync_parent_dir_succeeds_for_existing_file() {
        let tmp = NamedTempFile::new().expect("创建临时文件失败");
        let result = sync_parent_dir(tmp.path());
        assert!(
            result.is_ok(),
            "已存在文件的父目录 sync 应成功,实际: {:?}",
            result.err()
        );
    }

    /// 验证:对不存在的路径调用 `sync_parent_dir` 应返回 Err。
    ///
    /// 行为:
    /// 1. 构造一个不存在的路径 `/no/such/dir/file.bin`(Unix)或 `C:\\no\\such\\dir\\file.bin`(Windows)
    /// 2. 调用 `sync_parent_dir(path)`
    /// 3. 期望 Err(父目录无法打开 → NotFound 或 PermissionDenied)
    #[test]
    fn test_sync_parent_dir_returns_err_for_nonexistent_path() {
        let nonexistent = Path::new("/no/such/dir/file.bin");
        let result = sync_parent_dir(nonexistent);
        assert!(result.is_err(), "不存在的路径应返回 Err,实际返回 Ok");
    }

    /// 验证:对根路径调用 `sync_parent_dir` 不应 panic(平台相关行为放宽)。
    ///
    /// 行为:
    /// 1. 构造根路径:Unix `/`,Windows `C:\\`
    /// 2. 调用 `sync_parent_dir(root)`
    /// 3. 放宽断言:仅要求不 panic(结果 Ok 或 Err 均可,因根路径权限/语义平台相关)
    ///
    /// 注:根路径的 `parent()` 在 std 中返回 `None`,实现 MUST 处理该边界
    /// (返回 Ok 或特定的 Err,而非 panic)。
    #[test]
    fn test_sync_parent_dir_handles_root_no_panic() {
        let root = if cfg!(target_os = "windows") {
            Path::new("C:\\")
        } else {
            Path::new("/")
        };
        // 放宽断言:仅要求不 panic
        let _ = sync_parent_dir(root);
    }
}

// ── 实现契约提示(给 Implement Agent) ──
//
// 函数签名:
//   pub fn sync_parent_dir(path: &Path) -> std::io::Result<()>
//
// 导出位置:在 lib.rs 增加
//   pub mod dir_sync;
//   pub use dir_sync::sync_parent_dir;  // 或在 dir_sync.rs 内 `pub fn`
//
// 实现逻辑(参考 crates/tachyon-store/src/store.rs::sync_directory,已存在):
// 1. 取 `path.parent()`,若为 None(根路径)则返回 Ok(())(无父目录可 sync)
// 2. 打开父目录:
//    - Unix:`std::fs::File::open(parent)` + `sync_all()`(目录可作为文件打开,fsync 落盘目录项)
//    - Windows:`OpenOptions::new().read(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS)`
//      + `.share_mode(0x07)`,跳过 sync_all(NTFS rename 原子 + 日志保证目录项持久)
// 3. 失败时返回 Err,调用方决定是否传播
//
// 关键约束:
// - MUST 在 lib.rs 导出 `pub fn sync_parent_dir(path: &Path) -> std::io::Result<()>`
// - 函数应为同步函数(打开目录 + fsync 是阻塞 syscall,调用方应在 close 的
//   spawn_blocking 上下文中调用)
// - Windows MUST 用 FILE_FLAG_BACKUP_SEMANTICS(0x0200_0000),否则目录返回
//   ERROR_ACCESS_DENIED
// - Unix MUST 调用 `sync_all()`,Windows 跳过
// - 根路径(parent() = None)MUST 返回 Ok(()),不 panic
//
// 集成点:4 个后端的 `close()` 末尾(`spawn_blocking` 闭包内,在
// `file.sync_data()` 之后)调用 `sync_parent_dir(&self.path)`:
// - crates/tachyon-io/src/tokio_file.rs `pub async fn close`(unix cfg 路径)
// - crates/tachyon-io/src/winio.rs `pub async fn close`(Windows)
// - crates/tachyon-io/src/iocp.rs `pub async fn close` / trait close(Windows)
// - crates/tachyon-io/src/iouring.rs `fn close`(trait,Linux)
//
// 验证命令:
//   cargo check -p tachyon-io --lib --tests    # 实现前:E0425 cannot find function
//   cargo nextest run -p tachyon-io -- dir_sync                # 实现后应通过
//   cargo nextest run -p tachyon-io -- close_syncs_parent       # 实现后应通过
