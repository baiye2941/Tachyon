//! IOCP 存储引擎 (Windows only)
//!
//! # I/O 完成端口设计
//!
//! ```text
//! 网络收包 ──> IOCP 提交队列 ──> 完成通知 ──> 文件写入
//! ```
//!
//! 核心机制:
//! 1. **I/O 完成端口**:Windows 原生异步 I/O 模型,通过内核级完成通知
//!    实现高并发文件操作,避免用户态轮询开销。
//! 2. **OVERLAPPED I/O**:所有文件操作通过 OVERLAPPED 结构提交,
//!    内核在 I/O 完成后通过完成端口通知应用层。
//! 3. **线程池绑定**:完成端口与固定数量的工作线程绑定,
//!    自动实现负载均衡,避免线程爆炸。
//!
//! # 平台兼容性
//!
//! - Windows:完整 IOCP 实现
//! - 其他平台:编译为空桩,构造函数返回 `Unsupported` 错误

#[cfg(target_os = "windows")]
use std::future::Future;
use std::path::{Path, PathBuf};
#[cfg(target_os = "windows")]
use std::pin::Pin;
#[cfg(target_os = "windows")]
#[cfg(target_os = "windows")]
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use bytes::Bytes;
use tachyon_core::{DownloadError, DownloadResult};

/// pending 写入上下文。
///
/// `data` 必须由完成注册表持有到内核完成通知抵达,避免调用方取消
/// `write_at` future 后提前释放传给 `WriteFile` 的缓冲区。
#[cfg(target_os = "windows")]
struct PendingWrite {
    completion: tokio::sync::oneshot::Sender<DownloadResult<usize>>,
    data: Bytes,
    overlapped: Box<KernelOverlapped>,
}

/// 完成回调注册表:OVERLAPPED 堆地址 -> pending 写入上下文
///
/// 每个异步 I/O 操作将自身的 OVERLAPPED 指针作为键注册,
/// 完成后由轮询线程查找、发送结果并释放缓冲区和 OVERLAPPED。
#[cfg(target_os = "windows")]
type CompletionRegistry = std::collections::HashMap<usize, PendingWrite>;

#[cfg(target_os = "windows")]
fn lock_completion_registry(
    registry: &parking_lot::Mutex<CompletionRegistry>,
) -> parking_lot::MutexGuard<'_, CompletionRegistry> {
    registry.lock()
}

#[cfg(target_os = "windows")]
struct PendingWriteCancelGuard {
    file_handle: usize,
    overlapped_key: usize,
    registry: std::sync::Arc<parking_lot::Mutex<CompletionRegistry>>,
    armed: bool,
}

#[cfg(target_os = "windows")]
impl PendingWriteCancelGuard {
    fn new(
        file_handle: windows_sys::Win32::Foundation::HANDLE,
        overlapped_key: usize,
        registry: std::sync::Arc<parking_lot::Mutex<CompletionRegistry>>,
    ) -> Self {
        Self {
            file_handle: file_handle as usize,
            overlapped_key,
            registry,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

#[cfg(target_os = "windows")]
impl Drop for PendingWriteCancelGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        let map = lock_completion_registry(&self.registry);
        if !map.contains_key(&self.overlapped_key) {
            return;
        }

        let file_handle = self.file_handle as windows_sys::Win32::Foundation::HANDLE;
        let overlapped = self.overlapped_key as *mut windows_sys::Win32::System::IO::OVERLAPPED;
        // Safety:
        // - file_handle 来自仍存活的 IoCpStorage 文件句柄
        // - overlapped_key 命中 registry,对应的 Box<KernelOverlapped> 仍由 registry 持有
        // - CancelIoEx 只请求取消该 pending I/O,不释放 OVERLAPPED 或缓冲区
        let ok = unsafe { windows_sys::Win32::System::IO::CancelIoEx(file_handle, overlapped) };
        if ok == 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(windows_sys::Win32::Foundation::ERROR_NOT_FOUND as i32) {
                tracing::warn!(
                    ptr = self.overlapped_key,
                    error = %err,
                    "取消 IOCP pending write 失败"
                );
            }
        }
    }
}

/// IOCP 引擎状态
///
/// 状态转换:Created -> Ready -> Closed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoCpState {
    /// 已创建,未初始化完成端口
    Created,
    /// 完成端口已就绪,可接受 I/O 请求
    Ready,
    /// 已关闭,不再接受 I/O 请求
    Closed,
}

// ── Windows 实现 ──────────────────────────────────────────────

/// Windows 内核 OVERLAPPED 结构(匹配内核实际布局)
///
/// windows-sys 0.59 的 OVERLAPPED 将 Anonymous 放在偏移 16(与 InternalHigh 分离),
/// 而内核期望 Offset/OffsetHigh 与 InternalHigh 重叠(偏移 8/12)。
/// 此结构使用 #[repr(C)] 保证字段布局与 Windows SDK 定义一致。
#[cfg(target_os = "windows")]
#[repr(C)]
struct KernelOverlapped {
    internal: usize,
    internal_high: usize,
    offset_low: u32,
    offset_high: u32,
    h_event: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(target_os = "windows")]
impl KernelOverlapped {
    /// 创建零初始化的 OVERLAPPED,设置文件偏移
    fn new_for_offset(offset: u64) -> Self {
        Self {
            internal: 0,
            internal_high: 0,
            offset_low: offset as u32,
            offset_high: (offset >> 32) as u32,
            h_event: std::ptr::null_mut(),
        }
    }

    /// 重置为可复用状态,设置新的文件偏移
    fn reset(&mut self, offset: u64) {
        self.internal = 0;
        self.internal_high = 0;
        self.offset_low = offset as u32;
        self.offset_high = (offset >> 32) as u32;
        // h_event 保持 null_mut(),IOCP 不需要事件句柄
    }

    /// 获取内核兼容的 OVERLAPPED 指针
    fn as_overlapped_ptr(&mut self) -> *mut windows_sys::Win32::System::IO::OVERLAPPED {
        self as *mut Self as *mut windows_sys::Win32::System::IO::OVERLAPPED
    }
}

/// OVERLAPPED 对象池
///
/// IOCP 每次写入都需要一个 OVERLAPPED 结构;频繁 `Box::new` 分配与释放
/// 会成为高并发写入路径上的 GC/分配热点。对象池允许在完成通知到达后
/// 回收并复用 OVERLAPPED,显著降低 Windows 下载路径上的堆分配压力。
#[cfg(target_os = "windows")]
struct OverlappedPool {
    available: crossbeam_queue::SegQueue<Box<KernelOverlapped>>,
}

#[cfg(target_os = "windows")]
impl OverlappedPool {
    fn new() -> Self {
        Self {
            available: crossbeam_queue::SegQueue::new(),
        }
    }

    /// 获取一个 OVERLAPPED,优先从池中复用
    fn acquire(&self, offset: u64) -> Box<KernelOverlapped> {
        if let Some(mut ov) = self.available.pop() {
            ov.reset(offset);
            ov
        } else {
            Box::new(KernelOverlapped::new_for_offset(offset))
        }
    }

    /// 归还 OVERLAPPED 到池中复用
    fn release(&self, mut ov: Box<KernelOverlapped>) {
        ov.reset(0);
        self.available.push(ov);
    }
}

// Safety: KernelOverlapped 是提交给 Windows 内核的 POD 状态块,实际内存由
// PendingWrite 中的 Box 固定在堆上,只在完成通知抵达后由 poller 线程释放。
// 结构体本身没有 Rust 引用字段,跨线程移动所有权不会破坏别名规则。
#[cfg(target_os = "windows")]
unsafe impl Send for KernelOverlapped {}

/// IOCP 存储引擎 (Windows)
///
/// 基于 Windows I/O 完成端口的异步文件存储实现。
/// 仅分配结构体,不初始化完成端口。需要调用 `init()` 完成初始化。
#[cfg(target_os = "windows")]
pub struct IoCpStorage {
    /// 目标文件路径
    path: PathBuf,
    /// 当前引擎状态
    state: IoCpState,
    /// OVERLAPPED 文件句柄(通过 OpenOptionsExt 设置 FILE_FLAG_OVERLAPPED)
    file: Option<std::fs::File>,
    /// IOCP 句柄(即 windows_sys 的 HANDLE 类型 *mut c_void)
    port: Option<*mut std::ffi::c_void>,
    /// IOCP 轮询线程句柄
    poller: Option<std::thread::JoinHandle<()>>,
    /// 轮询线程退出信号(false=继续运行,true=请求退出)
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// 完成回调注册表:OVERLAPPED 堆地址 -> oneshot Sender
    ///
    /// 由 parking_lot::Mutex 保护,写入操作注册 Sender,轮询线程完成时取出。
    /// 使用 parking_lot 降低 Windows 高并发写入时的锁竞争开销。
    registry: std::sync::Arc<parking_lot::Mutex<CompletionRegistry>>,
    /// OVERLAPPED 对象池
    ///
    /// 复用已完成 I/O 的 OVERLAPPED 结构,减少高并发写入时的堆分配。
    overlapped_pool: std::sync::Arc<OverlappedPool>,
}

// Safety: IoCpStorage 的所有字段均可安全跨线程共享:
// - port (*mut c_void):Windows IOCP 句柄可在任意线程调用(内核保证线程安全)
// - file:Rust File 本身是 Send+Sync,通过 raw handle 访问时受 IOCP 调度保护
// - 其余字段均为 Arc/AtomicBool 等已知线程安全类型
#[cfg(target_os = "windows")]
unsafe impl Send for IoCpStorage {}
#[cfg(target_os = "windows")]
unsafe impl Sync for IoCpStorage {}

#[cfg(target_os = "windows")]
impl IoCpStorage {
    /// 创建新的 IOCP 存储引擎实例
    ///
    /// 仅分配结构体,不初始化完成端口。需要调用 `init()` 完成初始化。
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            state: IoCpState::Created,
            file: None,
            port: None,
            poller: None,
            shutdown: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            registry: std::sync::Arc::new(
                parking_lot::Mutex::new(std::collections::HashMap::new()),
            ),
            overlapped_pool: std::sync::Arc::new(OverlappedPool::new()),
        }
    }

    /// 获取当前引擎状态
    pub fn state(&self) -> IoCpState {
        self.state
    }

    /// 获取目标文件路径
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn clone_ready_file(&self) -> DownloadResult<std::fs::File> {
        if self.state != IoCpState::Ready {
            return Err(DownloadError::Io(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "IOCP 存储引擎未初始化",
            )));
        }

        self.file
            .as_ref()
            .ok_or_else(|| {
                DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "IOCP 文件句柄未初始化",
                ))
            })?
            .try_clone()
            .map_err(DownloadError::Io)
    }

    /// 初始化完成端口
    ///
    /// 流程:
    /// 1. 以 FILE_FLAG_OVERLAPPED 方式打开目标文件
    /// 2. 创建 I/O 完成端口并关联文件
    /// 3. 启动轮询线程循环调用 GetQueuedCompletionStatusEx
    /// 4. 状态 Created -> Ready
    pub fn init(&mut self) -> DownloadResult<()> {
        if self.state != IoCpState::Created {
            return Err(DownloadError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("IOCP 已初始化,当前状态: {:?}", self.state),
            )));
        }

        use std::os::windows::fs::OpenOptionsExt;

        // FILE_FLAG_OVERLAPPED(0x40000000) | FILE_FLAG_SEQUENTIAL_SCAN(0x08000000)
        // Safety: 这两个标志仅改变内核 I/O 调度行为,不涉及内存安全。
        const OVERLAPPED_SEQUENTIAL: u32 = 0x40000000 | 0x08000000;

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(OVERLAPPED_SEQUENTIAL)
            .open(&self.path)
            .map_err(DownloadError::Io)?;

        use std::os::windows::io::AsRawHandle;
        // Safety: file 是合法的 File 句柄,as_raw_handle() 返回内核分配的 HANDLE
        let file_handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;

        // 创建完成端口并关联文件句柄。
        // Safety:
        // - file_handle 来自合法的 OpenOptions::open(),是有效的文件句柄
        // - ExistingCompletionPort=null 表示创建新的完成端口并关联 file_handle
        // - CompletionKey=0 不使用键关联(通过 OVERLAPPED 获取上下文)
        // - NumberOfConcurrentThreads=0 让系统根据 CPU 核心数自动选择
        let port_handle = unsafe {
            windows_sys::Win32::System::IO::CreateIoCompletionPort(
                file_handle,
                std::ptr::null_mut(),
                0,
                0,
            )
        };
        if port_handle.is_null() {
            return Err(DownloadError::Io(std::io::Error::last_os_error()));
        }

        // 同步完成的 WriteFile 直接返回结果,不再向完成端口投递包,
        // 避免 fast path 已释放 OVERLAPPED 后 poller 再收到完成事件。
        const FILE_SKIP_COMPLETION_PORT_ON_SUCCESS: u8 = 1;
        // Safety:
        // - file_handle 已成功关联 IOCP
        // - 标志值来自 Windows FILE_SKIP_COMPLETION_PORT_ON_SUCCESS 常量
        let notification_mode_set = unsafe {
            windows_sys::Win32::Storage::FileSystem::SetFileCompletionNotificationModes(
                file_handle,
                FILE_SKIP_COMPLETION_PORT_ON_SUCCESS,
            )
        };
        if notification_mode_set == 0 {
            // Safety: port_handle 是上面成功创建的合法 IOCP 句柄
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(port_handle);
            }
            return Err(DownloadError::Io(std::io::Error::last_os_error()));
        }

        let shutdown_flag = self.shutdown.clone();
        let registry = self.registry.clone();
        let overlapped_pool = self.overlapped_pool.clone();
        // 通过 usize 传递句柄到线程(*mut c_void 不实现 Send)
        let port_raw = port_handle as usize;

        // 启动 IOCP 轮询线程
        let poller = match std::thread::Builder::new()
            .name("iocp-poller".into())
            .spawn(move || {
                // Safety: port_raw 来自成功的 CreateIoCompletionPort,转换回 HANDLE 安全
                let port = port_raw as windows_sys::Win32::Foundation::HANDLE;
                Self::poller_loop(port, &shutdown_flag, &registry, &overlapped_pool);
            }) {
            Ok(poller) => poller,
            Err(error) => {
                // Safety: port_handle 是上面成功创建的合法 IOCP 句柄。
                unsafe {
                    windows_sys::Win32::Foundation::CloseHandle(port_handle);
                }
                return Err(DownloadError::Io(error));
            }
        };

        self.file = Some(file);
        self.port = Some(port_handle);
        self.poller = Some(poller);
        self.state = IoCpState::Ready;

        tracing::info!(
            path = %self.path.display(),
            "IOCP 完成端口初始化成功"
        );

        Ok(())
    }

    /// IOCP 轮询线程主循环
    ///
    /// 循环调用 GetQueuedCompletionStatusEx 获取完成事件,
    /// 将结果通过 oneshot 通道分发到等待中的异步任务。
    /// 通过 shutdown 标志实现优雅退出。
    fn poller_loop(
        port: windows_sys::Win32::Foundation::HANDLE,
        shutdown: &std::sync::Arc<std::sync::atomic::AtomicBool>,
        registry: &std::sync::Arc<parking_lot::Mutex<CompletionRegistry>>,
        overlapped_pool: &std::sync::Arc<OverlappedPool>,
    ) {
        use windows_sys::Win32::System::IO::OVERLAPPED_ENTRY;

        // Safety: OVERLAPPED_ENTRY 是 POD 类型,全零初始化有效
        let mut entries: [OVERLAPPED_ENTRY; 16] = unsafe { std::mem::zeroed() };
        let mut num_entries: u32 = 0;

        loop {
            if shutdown.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }

            // Safety:
            // - port 是合法的 IOCP 句柄(创建时已验证)
            // - entries 数组地址有效且生命周期覆盖整个调用
            // - lpNumberOfBytesTransferred 指向合法的栈变量
            // - dwMilliseconds=100 让线程每 100ms 检查 shutdown 标志
            // - fAlertable=0 不使用 APC
            let ok = unsafe {
                windows_sys::Win32::System::IO::GetQueuedCompletionStatusEx(
                    port,
                    entries.as_mut_ptr(),
                    entries.len() as u32,
                    &mut num_entries,
                    100, // 100ms 超时,用于定期检查 shutdown 标志
                    0,   // fAlertable = FALSE
                )
            };

            if ok != 0 && num_entries > 0 {
                tracing::debug!(count = num_entries, "IOCP 完成事件");

                // 分发完成事件到等待中的 oneshot 通道
                for entry in &entries[..num_entries as usize] {
                    // lpOverlapped 来自内核完成通知。只有注册表命中的指针
                    // 才属于本模块提交的 write_at;未知指针不能释放。
                    let overlapped_ptr = entry.lpOverlapped as usize;
                    let bytes = entry.dwNumberOfBytesTransferred as usize;
                    // Internal 字段是 NTSTATUS; 0 = STATUS_SUCCESS
                    let status = entry.Internal as i32;

                    Self::complete_pending_write(
                        registry.as_ref(),
                        overlapped_pool.as_ref(),
                        overlapped_ptr,
                        bytes,
                        status,
                    );
                }
            }
            // ok == 0 且 GetLastError == WAIT_TIMEOUT:正常超时,继续循环
        }

        tracing::debug!("IOCP 轮询线程退出");
    }

    fn complete_pending_write(
        registry: &parking_lot::Mutex<CompletionRegistry>,
        overlapped_pool: &OverlappedPool,
        overlapped_ptr: usize,
        bytes: usize,
        status: i32,
    ) -> bool {
        let pending = {
            let mut map = lock_completion_registry(registry);
            map.remove(&overlapped_ptr)
        };

        let Some(pending) = pending else {
            tracing::warn!(
                ptr = overlapped_ptr,
                "IOCP 完成事件无对应注册,跳过未知 OVERLAPPED"
            );
            return false;
        };

        let result = if status == 0 {
            Ok(bytes)
        } else {
            Err(map_ntstatus_error(status))
        };
        let PendingWrite {
            completion,
            data,
            overlapped,
        } = pending;
        let _ = completion.send(result);
        drop(data);
        // 将 OVERLAPPED 归还对象池复用,避免下次写入重新堆分配
        overlapped_pool.release(overlapped);
        true
    }

    fn pending_count(&self) -> usize {
        lock_completion_registry(&self.registry).len()
    }

    fn cancel_pending_operations(&self) {
        if self.pending_count() == 0 {
            return;
        }

        let Some(file) = self.file.as_ref() else {
            return;
        };

        use std::os::windows::io::AsRawHandle;
        let file_handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
        // Safety:
        // - file_handle 来自仍由 self.file 持有的合法文件句柄
        // - lpOverlapped=null 表示取消该文件句柄上的所有 pending I/O
        let ok = unsafe {
            windows_sys::Win32::System::IO::CancelIoEx(file_handle, std::ptr::null_mut())
        };
        if ok == 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(windows_sys::Win32::Foundation::ERROR_NOT_FOUND as i32) {
                tracing::warn!(error = %err, "取消 IOCP pending I/O 失败");
            }
        }
    }

    fn drain_pending_completions(&self) -> usize {
        const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
        const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(10);

        let deadline = Instant::now() + DRAIN_TIMEOUT;
        loop {
            let pending = self.pending_count();
            if pending == 0 || Instant::now() >= deadline {
                return pending;
            }
            std::thread::sleep(DRAIN_POLL_INTERVAL);
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for IoCpStorage {
    fn drop(&mut self) {
        let pending = self.pending_count();
        if pending > 0 {
            tracing::warn!(
                pending,
                "IOCP drop 检测到 pending I/O,开始取消并等待完成通知"
            );
            self.cancel_pending_operations();
            let remaining = self.drain_pending_completions();
            if remaining > 0 {
                tracing::error!(
                    remaining,
                    "IOCP pending I/O 未在超时内完成,泄漏 registry 以避免释放内核仍可能使用的缓冲区"
                );
                std::mem::forget(self.registry.clone());
            }
        }

        // 1. 请求轮询线程退出
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Release);

        // 2. 关闭 IOCP 端口,让 GetQueuedCompletionStatusEx 返回错误退出循环
        if let Some(port) = self.port.take() {
            // Safety: port 值来自成功的 CreateIoCompletionPort,是合法的 IOCP 句柄
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(port);
            }
        }

        // 3. 等待轮询线程退出
        if let Some(handle) = self.poller.take() {
            let _ = handle.join();
        }

        // 4. 文件句柄在 self.file drop 时自动关闭

        if self.state != IoCpState::Closed {
            self.state = IoCpState::Closed;
        }
    }
}

#[cfg(target_os = "windows")]
impl crate::storage::AsyncStorage for IoCpStorage {
    /// 通过 IOCP 完成端口提交异步写入
    ///
    /// 流程:
    /// 1. 使用 KernelOverlapped(内核兼容布局)提交 WriteFile
    /// 2. pending I/O 由 poller 线程从完成端口接收通知
    /// 3. 同步完成时直接返回,不会再投递完成包
    fn write_at(
        &self,
        offset: u64,
        data: Bytes,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
        Box::pin(async move {
            if self.state != IoCpState::Ready {
                return Err(DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "IOCP 存储引擎未初始化",
                )));
            }

            use std::os::windows::io::AsRawHandle;

            // 1. 从对象池获取 OVERLAPPED,减少高并发写入时的堆分配。
            // 复用的 OVERLAPPED 会由 registry 持有直到完成通知抵达,再归还池中。
            let mut overlapped = self.overlapped_pool.acquire(offset);
            let ov_ptr = overlapped.as_overlapped_ptr();

            let file = self.file.as_ref().ok_or_else(|| {
                DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "IOCP 文件句柄未初始化",
                ))
            })?;
            let file_handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
            let overlapped_key = ov_ptr as usize;
            let data_ptr = data.as_ptr();
            let data_len = data.len();
            let (tx, rx) = tokio::sync::oneshot::channel();
            {
                let mut map = lock_completion_registry(&self.registry);
                map.insert(
                    overlapped_key,
                    PendingWrite {
                        completion: tx,
                        data,
                        overlapped,
                    },
                );
            }

            let mut bytes_written: u32 = 0;
            // Safety:
            // - file_handle 来自 init() 中 FILE_FLAG_OVERLAPPED 打开的文件
            // - data_ptr 指向 registry 中 PendingWrite 持有的 Bytes 缓冲区
            // - ov_ptr 指向 registry 中 PendingWrite 持有的 KernelOverlapped
            let write_ok = unsafe {
                windows_sys::Win32::Storage::FileSystem::WriteFile(
                    file_handle,
                    data_ptr,
                    data_len as u32,
                    &mut bytes_written,
                    ov_ptr,
                )
            };

            if write_ok != 0 {
                // 同步完成(fast path):WriteFile 直接写入成功
                tracing::debug!(bytes = bytes_written, "IOCP write_at 同步完成");
                {
                    let mut map = lock_completion_registry(&self.registry);
                    if let Some(pending) = map.remove(&overlapped_key) {
                        self.overlapped_pool.release(pending.overlapped);
                    }
                }
                return Ok(bytes_written as usize);
            }

            // 4. 检查是否为 ERROR_IO_PENDING(异步进行中)
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(windows_sys::Win32::Foundation::ERROR_IO_PENDING as i32) {
                // 真正的写入失败
                {
                    let mut map = lock_completion_registry(&self.registry);
                    if let Some(pending) = map.remove(&overlapped_key) {
                        self.overlapped_pool.release(pending.overlapped);
                    }
                }
                return Err(map_writefile_submission_error(err));
            }

            // pending I/O 的缓冲区和 OVERLAPPED 继续由 registry 持有到完成通知。
            let mut cancel_guard =
                PendingWriteCancelGuard::new(file_handle, overlapped_key, self.registry.clone());
            let completion = rx.await.map_err(|_| {
                DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "IOCP 完成通知通道关闭",
                ))
            });
            cancel_guard.disarm();
            completion?
        })
    }

    fn read_at<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
        Box::pin(async move {
            use std::os::windows::fs::FileExt;

            let file = self.clone_ready_file()?;
            let buf_len = buf.len();
            let mut owned_buf = vec![0u8; buf_len];
            let (n, owned_buf) = tokio::task::spawn_blocking(move || {
                let n = file.seek_read(&mut owned_buf, offset)?;
                Ok::<_, std::io::Error>((n, owned_buf))
            })
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
            .map_err(DownloadError::Io)?;
            buf[..n].copy_from_slice(&owned_buf[..n]);
            Ok(n)
        })
    }

    fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        Box::pin(async move {
            let file = self.clone_ready_file()?;
            tokio::task::spawn_blocking(move || file.sync_data().map_err(DownloadError::Io))
                .await
                .map_err(|e| DownloadError::Io(e.into()))?
        })
    }

    fn allocate(&self, size: u64) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        Box::pin(async move {
            let file = self.clone_ready_file()?;
            tokio::task::spawn_blocking(move || file.set_len(size).map_err(DownloadError::Io))
                .await
                .map_err(|e| DownloadError::Io(e.into()))?
        })
    }

    fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
        Box::pin(async move {
            let file = self.clone_ready_file()?;
            tokio::task::spawn_blocking(move || {
                file.metadata().map(|m| m.len()).map_err(DownloadError::Io)
            })
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
        })
    }

    fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        Box::pin(async move { self.sync().await })
    }
}

// ── 非 Windows 平台桩 ────────────────────────────────────────

/// IOCP 存储引擎 (非 Windows 平台桩)
///
/// IOCP 是 Windows 特有的 I/O 完成端口机制,
/// 在其他平台上仅提供空桩实现。
#[cfg(not(target_os = "windows"))]
pub struct IoCpStorage {
    path: PathBuf,
    state: IoCpState,
}

#[cfg(not(target_os = "windows"))]
impl IoCpStorage {
    /// 创建新的 IOCP 存储引擎实例
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            state: IoCpState::Created,
        }
    }

    /// 获取当前引擎状态
    pub fn state(&self) -> IoCpState {
        self.state
    }

    /// 获取目标文件路径
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 初始化完成端口(非 Windows 平台始终返回 Unsupported)
    pub fn init(&mut self) -> DownloadResult<()> {
        Err(DownloadError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "IOCP 仅支持 Windows 平台",
        )))
    }
}

#[cfg(not(target_os = "windows"))]
impl Drop for IoCpStorage {
    fn drop(&mut self) {
        self.state = IoCpState::Closed;
    }
}

// ── 错误映射 ────────────────────────────────────────────────

/// 将 Windows 错误码映射为 DownloadError
///
/// ADR-001 定义的映射规则:
/// - `ERROR_HANDLE_EOF` -> `Io(UnexpectedEof)`
/// - `ERROR_ACCESS_DENIED` -> `Forbidden { status: 403 }`
/// - `ERROR_DISK_FULL` -> `Io(StorageFull)`
/// - `ERROR_OPERATION_ABORTED` -> `Cancelled`
/// - `ERROR_IO_INCOMPLETE` / `ERROR_IO_PENDING` -> `Io(WouldBlock)` (内部状态,不暴露)
/// - 其他 Win32 错误 -> `from_raw_os_error`
#[cfg(target_os = "windows")]
fn map_windows_error(code: u32) -> DownloadError {
    use windows_sys::Win32::Foundation::*;
    match code {
        ERROR_HANDLE_EOF => {
            DownloadError::Io(std::io::Error::from(std::io::ErrorKind::UnexpectedEof))
        }
        ERROR_ACCESS_DENIED => DownloadError::Forbidden { status: 403 },
        ERROR_DISK_FULL => DownloadError::Io(std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "磁盘空间不足",
        )),
        ERROR_OPERATION_ABORTED => DownloadError::Cancelled,
        ERROR_IO_INCOMPLETE | ERROR_IO_PENDING => {
            // 内部状态,不暴露给调用方
            DownloadError::Io(std::io::Error::from(std::io::ErrorKind::WouldBlock))
        }
        _ => DownloadError::Io(std::io::Error::from_raw_os_error(code as i32)),
    }
}

#[cfg(target_os = "windows")]
fn map_writefile_submission_error(error: std::io::Error) -> DownloadError {
    if let Some(code) = error.raw_os_error() {
        map_windows_error(code as u32)
    } else {
        DownloadError::Io(error)
    }
}

#[cfg(target_os = "windows")]
fn map_ntstatus_error(status: i32) -> DownloadError {
    // Safety: status 来自 IOCP OVERLAPPED_ENTRY::Internal 的 NTSTATUS 值,
    // RtlNtStatusToDosError 只做系统错误码转换,不持有指针或外部资源。
    let win32_code = unsafe { windows_sys::Win32::Foundation::RtlNtStatusToDosError(status) };
    map_windows_error(win32_code)
}

// ── 测试 ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 IOCP 初始化后状态转换为 Ready
    #[test]
    fn test_iocp_init_state_ready() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut storage = IoCpStorage::new(tmp.path());
        assert_eq!(storage.state(), IoCpState::Created);

        let result = storage.init();

        #[cfg(target_os = "windows")]
        {
            result.expect("IOCP init 应在 Windows 上成功");
            assert_eq!(storage.state(), IoCpState::Ready);
        }

        #[cfg(not(target_os = "windows"))]
        {
            assert!(result.is_err(), "非 Windows 应返回错误");
        }
    }

    /// 验证重复初始化返回错误
    #[test]
    fn test_iocp_init_twice_errors() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut storage = IoCpStorage::new(tmp.path());
        let _ = storage.init();

        // 第二次 init 应失败(Windows=AlreadyExists,非 Windows=Unsupported)
        let result = storage.init();
        assert!(result.is_err(), "重复初始化应返回错误");
    }

    /// 验证非 Windows 平台返回 Unsupported
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn test_iocp_init_non_windows_returns_error() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut storage = IoCpStorage::new(tmp.path());
        let err = storage.init().unwrap_err();
        assert!(
            err.to_string().contains("仅支持 Windows"),
            "错误信息应说明平台不支持: {err}"
        );
    }

    /// 验证构造后路径和初始状态
    #[test]
    fn test_iocp_new_defaults() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let storage = IoCpStorage::new(tmp.path());
        assert_eq!(storage.state(), IoCpState::Created);
        assert_eq!(storage.path(), tmp.path());
    }

    /// 验证 Drop 将状态设为 Closed(不 panic)
    #[test]
    fn test_iocp_drop_sets_closed() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut storage = IoCpStorage::new(tmp.path());
        let _ = storage.init();
        // Drop 触发时应设为 Closed,若 panic 则测试失败
        drop(storage);
    }

    // ── write_at 测试 (Windows only,需要 tokio runtime) ────────

    /// 验证未知 completion 不会被当作本模块拥有的 OVERLAPPED 释放。
    #[cfg(target_os = "windows")]
    #[test]
    fn test_iocp_unknown_completion_is_not_owned() {
        let registry = parking_lot::Mutex::new(std::collections::HashMap::new());
        let pool = OverlappedPool::new();

        assert!(
            !IoCpStorage::complete_pending_write(&registry, &pool, 0xDEAD_BEEF, 0, 0),
            "未知 completion 应被忽略,不能释放外部 OVERLAPPED"
        );
        assert_eq!(lock_completion_registry(&registry).len(), 0);
    }

    /// 验证注册表命中的 completion 才会移除 pending 并发送写入结果。
    #[cfg(target_os = "windows")]
    #[test]
    fn test_iocp_registered_completion_resolves_pending_write() {
        let registry = parking_lot::Mutex::new(std::collections::HashMap::new());
        let pool = OverlappedPool::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut overlapped = Box::new(KernelOverlapped::new_for_offset(0));
        let key = overlapped.as_overlapped_ptr() as usize;

        {
            let mut map = lock_completion_registry(&registry);
            map.insert(
                key,
                PendingWrite {
                    completion: tx,
                    data: Bytes::from_static(b"abc"),
                    overlapped,
                },
            );
        }

        assert!(
            IoCpStorage::complete_pending_write(&registry, &pool, key, 3, 0),
            "注册表命中的 completion 应完成 pending write"
        );
        assert_eq!(lock_completion_registry(&registry).len(), 0);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let written = rt
            .block_on(rx)
            .expect("completion sender 应发送结果")
            .expect("status=0 应映射为成功");
        assert_eq!(written, 3);
    }

    /// 验证 pending write 的取消 guard 只请求取消,不抢占 registry 所有权。
    #[cfg(target_os = "windows")]
    #[test]
    fn test_iocp_cancel_guard_preserves_pending_registry_entry() {
        let registry =
            std::sync::Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new()));
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let mut overlapped = Box::new(KernelOverlapped::new_for_offset(0));
        let key = overlapped.as_overlapped_ptr() as usize;

        {
            let mut map = lock_completion_registry(&registry);
            map.insert(
                key,
                PendingWrite {
                    completion: tx,
                    data: Bytes::from_static(b"cancel"),
                    overlapped,
                },
            );
        }

        {
            let _guard = PendingWriteCancelGuard::new(std::ptr::null_mut(), key, registry.clone());
        }

        let mut map = lock_completion_registry(&registry);
        assert!(
            map.contains_key(&key),
            "取消 guard 不能移除 pending,必须等 completion poller 统一释放"
        );
        map.remove(&key);
    }

    /// 验证基本写入:写入 4096 字节并确认返回正确字节数
    #[cfg(target_os = "windows")]
    #[test]
    fn test_iocp_write_at_basic() {
        use crate::storage::AsyncStorage;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_iocp_basic.dat");
        let mut storage = IoCpStorage::new(&path);
        storage.init().expect("IOCP init 应成功");

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let data = bytes::Bytes::from(vec![0xABu8; 4096]);
            let written = storage.write_at(0, data).await.expect("write_at 应成功");
            assert_eq!(written, 4096, "写入字节数应为 4096");
        });
    }

    /// 验证未初始化时写入返回 NotConnected 错误
    #[cfg(target_os = "windows")]
    #[test]
    fn test_iocp_write_at_not_ready() {
        use crate::storage::AsyncStorage;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let storage = IoCpStorage::new(tmp.path());
        // 不调用 init(),状态为 Created

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let data = bytes::Bytes::from(vec![0xABu8; 1024]);
            let result = storage.write_at(0, data).await;
            assert!(result.is_err(), "未初始化时写入应返回错误");
            let err = result.unwrap_err();
            assert!(
                err.to_string().contains("未初始化"),
                "错误信息应包含'未初始化': {err}"
            );
        });
    }

    /// 验证指定偏移写入:先写 offset=0,再写 offset=4096,读回验证
    #[cfg(target_os = "windows")]
    #[test]
    fn test_iocp_write_at_offset() {
        use crate::storage::AsyncStorage;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut storage = IoCpStorage::new(tmp.path());
        storage.init().expect("IOCP init 应成功");

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // 写入偏移 0
            let data_a = bytes::Bytes::from(vec![0xAAu8; 4096]);
            let written_a = storage
                .write_at(0, data_a)
                .await
                .expect("write_at(0) 应成功");
            assert_eq!(written_a, 4096);

            // 写入偏移 4096
            let data_b = bytes::Bytes::from(vec![0xBBu8; 4096]);
            let written_b = storage
                .write_at(4096, data_b)
                .await
                .expect("write_at(4096) 应成功");
            assert_eq!(written_b, 4096);

            // 读回文件验证数据正确性
            // (使用同步 std::io 读取,IOCP 仅用于写入)
            let mut buf = vec![0u8; 8192];
            let mut f = std::fs::File::open(tmp.path()).expect("应能打开临时文件");
            use std::io::Read;
            f.read_exact(&mut buf).expect("应能读取完整内容");

            // 前 4096 字节应为 0xAA
            assert!(
                buf[..4096].iter().all(|&b| b == 0xAA),
                "偏移 0~4095 应为 0xAA"
            );
            // 后 4096 字节应为 0xBB
            assert!(
                buf[4096..8192].iter().all(|&b| b == 0xBB),
                "偏移 4096~8191 应为 0xBB"
            );
        });
    }

    // ── Windows 错误映射测试 ────────────────────────────────

    /// 验证 EOF 错误映射为 Io(UnexpectedEof)
    #[cfg(target_os = "windows")]
    #[test]
    fn test_iocp_error_mapping_eof() {
        use windows_sys::Win32::Foundation::ERROR_HANDLE_EOF;
        assert!(matches!(
            map_windows_error(ERROR_HANDLE_EOF),
            DownloadError::Io(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof
        ));
    }

    /// 验证 ACCESS_DENIED 映射为 Forbidden { status: 403 }
    #[cfg(target_os = "windows")]
    #[test]
    fn test_iocp_error_mapping_access_denied() {
        use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
        assert!(matches!(
            map_windows_error(ERROR_ACCESS_DENIED),
            DownloadError::Forbidden { status: 403 }
        ));
    }

    /// 验证 DISK_FULL 映射为 Io(Other) 并包含磁盘空间提示
    #[cfg(target_os = "windows")]
    #[test]
    fn test_iocp_error_mapping_disk_full() {
        use windows_sys::Win32::Foundation::ERROR_DISK_FULL;
        let err = map_windows_error(ERROR_DISK_FULL);
        assert!(
            matches!(err, DownloadError::Io(ref e) if e.kind() == std::io::ErrorKind::StorageFull)
        );
        assert!(err.to_string().contains("磁盘空间不足"));
    }

    /// 验证 OPERATION_ABORTED 映射为 Cancelled
    #[cfg(target_os = "windows")]
    #[test]
    fn test_iocp_error_mapping_operation_aborted() {
        use windows_sys::Win32::Foundation::ERROR_OPERATION_ABORTED;
        assert!(matches!(
            map_windows_error(ERROR_OPERATION_ABORTED),
            DownloadError::Cancelled
        ));
    }

    /// 验证 WriteFile 直接失败路径也使用 ADR 定义的 Win32 错误映射。
    #[cfg(target_os = "windows")]
    #[test]
    fn test_iocp_writefile_submission_error_mapping() {
        use windows_sys::Win32::Foundation::{
            ERROR_ACCESS_DENIED, ERROR_DISK_FULL, ERROR_OPERATION_ABORTED,
        };

        assert!(matches!(
            map_writefile_submission_error(std::io::Error::from_raw_os_error(
                ERROR_ACCESS_DENIED as i32,
            )),
            DownloadError::Forbidden { status: 403 }
        ));

        assert!(matches!(
            map_writefile_submission_error(std::io::Error::from_raw_os_error(
                ERROR_DISK_FULL as i32,
            )),
            DownloadError::Io(ref error) if error.kind() == std::io::ErrorKind::StorageFull
        ));

        assert!(matches!(
            map_writefile_submission_error(std::io::Error::from_raw_os_error(
                ERROR_OPERATION_ABORTED as i32,
            )),
            DownloadError::Cancelled
        ));
    }

    /// 验证 IO_INCOMPLETE 和 IO_PENDING 映射为 Io(WouldBlock)
    #[cfg(target_os = "windows")]
    #[test]
    fn test_iocp_error_mapping_io_pending() {
        use windows_sys::Win32::Foundation::{ERROR_IO_INCOMPLETE, ERROR_IO_PENDING};
        assert!(matches!(
            map_windows_error(ERROR_IO_INCOMPLETE),
            DownloadError::Io(ref e) if e.kind() == std::io::ErrorKind::WouldBlock
        ));
        assert!(matches!(
            map_windows_error(ERROR_IO_PENDING),
            DownloadError::Io(ref e) if e.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    /// 验证未知错误码映射为 Io(from_raw_os_error)
    #[cfg(target_os = "windows")]
    #[test]
    fn test_iocp_error_mapping_unknown() {
        let err = map_windows_error(0xDEAD);
        assert!(matches!(err, DownloadError::Io(_)));
        if let DownloadError::Io(ref e) = err {
            assert_eq!(e.raw_os_error(), Some(0xDEAD));
        }
    }
}
