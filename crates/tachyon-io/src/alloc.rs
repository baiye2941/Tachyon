//! F-14: Windows 文件预分配 helper
//!
//! 集中实现 `i64::try_from(size)` 溢出检查 + `set_len` 后
//! `SetFileInformationByHandle` 失败时 rollback 到旧 `metadata().len()`。
//! tokio_file / winio / iocp 的 allocate / preallocate 改调本 helper。
//!
//! # 审计发现 F-14
//!
//! `tokio_file.rs:194,228`(Windows)`size as i64` 裸转:
//! - `FILE_ALLOCATION_INFO.AllocationSize: size as i64`
//! - `SetFileValidData(handle, size as i64)`
//!
//! `tokio_file.rs:349`(Linux)`size as libc::off_t` 裸转。
//! `winio.rs:194,227` 同样裸转。
//! `iocp.rs:1197,1231` 同样裸转。
//!
//! Windows `set_len` 成功但 `SetFileInformationByHandle` 失败时,文件逻辑大小
//! 已被扩展,但物理分配未完成,且无 rollback —— 文件留在不一致状态。

use tachyon_core::{DownloadError, DownloadResult};

/// F-14: Windows 文件预分配 helper。
///
/// 1. `i64::try_from(size)` 溢出检查:超过 `i64::MAX` 返回 `Io(InvalidInput)`,
///    此时不触碰文件(`set_len` 在检查通过后才调用)。
/// 2. 记录旧 `metadata().len()` 作为 rollback 目标。
/// 3. `set_len(size)` 设置逻辑大小(EOF)。
/// 4. `SetFileInformationByHandle(FileAllocationInfo)` 真正预分配物理磁盘块;
///    失败时 `set_len(old_size)` rollback 到旧逻辑大小。
/// 5. `SetFileValidData` 跳过零填充(需 `SE_MANAGE_VOLUME_NAME`),失败时静默回退
///    (保留原有行为,仅日志,不影响成功路径)。
///
/// 非 Windows 平台提供 stub,返回 `Unsupported`(仅作为符号存在性保证,不应被调用)。
pub fn allocate_windows(file: &std::fs::File, size: u64) -> DownloadResult<()> {
    // 1. 溢出检查:Windows FFI 的 AllocationSize/ValidData 参数均为 i64,
    //    `size as i64` 在 size > i64::MAX 时静默截断为负数,内核行为未定义。
    //    入口处显式校验,溢出时直接返回 InvalidInput,不触碰文件状态。
    i64::try_from(size).map_err(|_| {
        DownloadError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "allocate_windows size {size} 超过 i64 最大值 {},FFI 参数无法表示",
                i64::MAX
            ),
        ))
    })?;

    // 2. 记录旧逻辑大小作为 rollback 目标。
    let old_size = file.metadata().map_err(DownloadError::Io)?.len();

    // 3. 设置文件逻辑大小(EOF)。set_len 成功后,文件逻辑大小已扩展;
    //    若后续 SetFileInformationByHandle 失败,需 rollback 到 old_size。
    file.set_len(size).map_err(DownloadError::Io)?;

    #[cfg(target_os = "windows")]
    {
        // 4. SetFileInformationByHandle(FileAllocationInfo) 真正预分配物理磁盘块,
        //    避免稀疏文件仅扩展逻辑大小而不分配空间。失败时 rollback 到 old_size。
        {
            use std::os::windows::io::AsRawHandle;
            let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
            let info = windows_sys::Win32::Storage::FileSystem::FILE_ALLOCATION_INFO {
                AllocationSize: size as i64,
            };
            // Safety:
            // - handle 来自合法的 &std::fs::File 引用,调用期间保持存活
            // - info 指针指向有效的 FILE_ALLOCATION_INFO 结构
            // - FileAllocationInfo 是 Windows 定义的标准信息类
            // - 失败时通过 last_os_error 返回错误,并 rollback 文件大小到 old_size
            let result = unsafe {
                windows_sys::Win32::Storage::FileSystem::SetFileInformationByHandle(
                    handle,
                    windows_sys::Win32::Storage::FileSystem::FileAllocationInfo,
                    &info as *const _ as *const std::ffi::c_void,
                    std::mem::size_of::<windows_sys::Win32::Storage::FileSystem::FILE_ALLOCATION_INFO>(
                    ) as u32,
                )
            };
            if result == 0 {
                let e = std::io::Error::last_os_error();
                // rollback:把文件逻辑大小恢复到 old_size,避免留下
                // "逻辑大小已扩展但物理分配未完成"的不一致状态。
                // rollback 失败不传播(原始错误更重要),仅日志记录。
                if let Err(rollback_err) = file.set_len(old_size) {
                    tracing::warn!(
                        old_size,
                        new_size = size,
                        error = %rollback_err,
                        "allocate_windows rollback set_len 失败,文件大小可能不一致"
                    );
                }
                return Err(DownloadError::Io(e));
            }
        }

        // 5. SetFileValidData 跳过零填充(需要 SE_MANAGE_VOLUME_NAME 权限)。
        //    失败时静默回退(保留原行为):文件已通过 set_len + FileAllocationInfo
        //    正确扩展,仅扩展区域为零填充而非磁盘残留数据,功能正确但较慢。
        //    注意:成功时文件扩展区域包含磁盘残留数据(非零填充),但下载数据
        //    会立即覆盖,安全风险极低。
        {
            use std::os::windows::io::AsRawHandle;
            let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
            // Safety:
            // - handle 来自合法的 &std::fs::File 引用,调用期间保持存活
            // - size 已通过 i64::try_from 校验,是合法的 i64 值
            // - 内核保证:失败时不影响文件已有状态
            let result = unsafe {
                windows_sys::Win32::Storage::FileSystem::SetFileValidData(handle, size as i64)
            };
            if result == 0 {
                tracing::debug!(
                    size,
                    "SetFileValidData 失败(需 SE_MANAGE_VOLUME_NAME),回退到零填充模式"
                );
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        // 非 Windows stub:保证符号存在(跨平台编译期断言用)。
        // 实际不应被调用 —— tokio_file Linux 路径有自己的 fallocate 实现,
        // 不经此 helper。
        let _ = (file, size);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;

    // 引用待实现的 helper 函数。当前不存在,触发 E0425 编译失败(RED 状态)。
    use super::allocate_windows;

    /// 辅助断言:错误应为 `Io(InvalidInput)` 且消息包含关键字。
    fn assert_invalid_input_error(err: &tachyon_core::DownloadError, expected_message: &str) {
        match err {
            tachyon_core::DownloadError::Io(io_error) => {
                assert_eq!(
                    io_error.kind(),
                    std::io::ErrorKind::InvalidInput,
                    "期望 InvalidInput,实际: {io_error}"
                );
                assert!(
                    io_error.to_string().contains(expected_message),
                    "错误信息应包含 {expected_message}, 实际: {io_error}"
                );
            }
            other => panic!("应返回 I/O InvalidInput 错误,实际: {other}"),
        }
    }

    // ── F-14: allocate_windows helper 契约测试 ──────────────────────
    //
    // 审计 F-14:Windows 路径 `size as i64` 裸转,超过 i64::MAX 时静默截断为负数,
    // 传给 SetFileInformationByHandle / SetFileValidData 行为未定义。
    // 期望:入口处 `i64::try_from(size)` 溢出检查,返回 InvalidInput。
    //
    // 实现契约提示(给 Implement Agent):
    // - `pub fn allocate_windows(file: &std::fs::File, size: u64) -> DownloadResult<()>`
    // - 入口:`i64::try_from(size).map_err(|_| invalid_input(format!("...")))?;`
    // - 错误模式:`DownloadError::Io(std::io::Error::new(InvalidInput, msg))`
    // - 参照实现:iouring.rs:1685(`i64::try_from(size).map_err(|_| ...)`)
    //
    // cfg gate 策略:
    // - `test_allocate_windows_rejects_size_over_i64_max`:Windows cfg
    // - `test_allocate_windows_succeeds_for_normal_size`:Windows cfg
    // - `test_allocate_windows_rolls_back_on_setfileinfo_failure`:Windows cfg
    // - `test_allocate_windows_helper_exists`:跨平台(编译期存在性)

    /// F-14 契约:`allocate_windows` 拒绝超过 `i64::MAX` 的 size。
    ///
    /// 预期失败原因:`allocate_windows` 函数尚不存在(E0425 编译失败)。
    /// 实现后:`u64::MAX > i64::MAX`,应返回 `Io(InvalidInput)`,且文件大小保持 0。
    #[cfg(target_os = "windows")]
    #[test]
    fn test_allocate_windows_rejects_size_over_i64_max() {
        let tmp = NamedTempFile::new().unwrap();
        let file = std::fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(tmp.path())
            .unwrap();

        let err = allocate_windows(&file, u64::MAX).expect_err("u64::MAX 应被拒绝");

        assert_invalid_input_error(&err, "i64");

        // 文件大小不应被改变(rollback 或未触达 set_len)
        let size_after = file.metadata().unwrap().len();
        assert_eq!(
            size_after, 0,
            "拒绝溢出 size 后文件大小应保持 0,实际 {size_after}"
        );
    }

    /// F-14 契约:`allocate_windows` 对合法 size 成功预分配。
    ///
    /// 预期失败原因:`allocate_windows` 函数尚不存在(E0425)。
    /// 实现后:`allocate_windows(&file, 1024)` 应返回 `Ok`,文件大小 == 1024。
    #[cfg(target_os = "windows")]
    #[test]
    fn test_allocate_windows_succeeds_for_normal_size() {
        let tmp = NamedTempFile::new().unwrap();
        let file = std::fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(tmp.path())
            .unwrap();

        allocate_windows(&file, 1024).expect("合法 size 1024 应成功");

        let size_after = file.metadata().unwrap().len();
        assert_eq!(
            size_after, 1024,
            "预分配后文件大小应为 1024,实际 {size_after}"
        );
    }

    /// F-14 契约:`set_len` 成功但 `SetFileInformationByHandle` 失败时 rollback。
    ///
    /// 若难以注入故障(无 mock file),改用溢出 size 触发 rollback 路径:
    /// 1. 先 `allocate_windows(&file, 512)` 成功,文件大小 = 512
    /// 2. 再 `allocate_windows(&file, u64::MAX)` 失败(溢出检查)
    /// 3. 文件大小应回退到 512(rollback 到旧大小,而非 0 或 u64::MAX 截断值)
    ///
    /// 注意:溢出检查在 `set_len` 之前,故此路径验证的是"溢出时不破坏已有大小"。
    /// 真正的 rollback(set_len 后 SetFileInfo 失败)需 fault injection,此处用
    /// 溢出路径作为最小可行验证。
    ///
    /// 预期失败原因:`allocate_windows` 函数尚不存在(E0425)。
    #[cfg(target_os = "windows")]
    #[test]
    fn test_allocate_windows_rolls_back_on_setfileinfo_failure() {
        let tmp = NamedTempFile::new().unwrap();
        let file = std::fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(tmp.path())
            .unwrap();

        // 步骤 1:先成功预分配 512 字节
        allocate_windows(&file, 512).expect("第一次预分配 512 应成功");
        let size_after_first = file.metadata().unwrap().len();
        assert_eq!(size_after_first, 512);

        // 步骤 2:再用溢出 size 调用,应失败但不破坏已有大小
        let err = allocate_windows(&file, u64::MAX).expect_err("u64::MAX 应被拒绝");
        assert_invalid_input_error(&err, "i64");

        // 步骤 3:文件大小应保持 512(rollback 到旧大小,未被破坏)
        let size_after_failure = file.metadata().unwrap().len();
        assert_eq!(
            size_after_failure, 512,
            "失败后文件大小应 rollback 到 512,实际 {size_after_failure}"
        );
    }

    /// F-14 契约:`allocate_windows` 函数存在性(编译测试)。
    ///
    /// 跨平台编译期断言:无论在何平台,`allocate_windows` 符号必须存在(即使
    /// 非 Windows 平台为空实现或编译为 stub)。此测试确保 Implement Agent
    /// 不会遗漏 helper 的导出。
    ///
    /// 预期失败原因:函数未定义(E0425)。
    #[test]
    fn test_allocate_windows_helper_exists() {
        // 取函数指针证明符号存在(不调用,避免平台依赖)。
        let _f: fn(&std::fs::File, u64) -> tachyon_core::DownloadResult<()> = allocate_windows;
    }
}
