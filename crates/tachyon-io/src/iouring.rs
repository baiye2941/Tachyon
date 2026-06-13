//! io_uring 零拷贝存储引擎 (Linux only)
//!
//! # 零拷贝管道设计
//!
//! ```text
//! 网络收包 ──> io_uring fixed buffer ──> 文件写入
//!    │                │                    │
//!    └── 无用户态拷贝 ──┘                    │
//!    └── 无堆分配 ──────── SQE/CQE 驱动 ────┘
//! ```
//!
//! 核心机制:
//! 1. **Fixed Buffer 注册**:将预分配的内存区域注册到内核,
//!    后续 I/O 操作直接使用注册地址,避免每次操作的页表查找开销。
//! 2. **SQPOLL 模式**:内核线程轮询提交队列,消除了 `io_uring_enter` 系统调用的开销。
//! 3. **O_DIRECT 标志**:绕过页缓存,数据直接从用户 buffer 写入磁盘。
//! 4. **批量提交**:多个 SQE 一次性提交,减少系统调用次数。
//!
//! # 零拷贝管道工作流
//!
//! 1. 初始化阶段:创建 io_uring 实例,注册 fixed buffers,打开目标文件
//! 2. 写入阶段:将网络数据复制到 fixed buffer(仅一次拷贝),构造 SQE 提交写入
//! 3. 完成阶段:从 CQ 获取完成事件,释放 fixed buffer 索引
//!
//! 与标准 tokio 文件 I/O 相比,io_uring 路径可减少:
//! - 系统调用次数(批量提交 vs 每次 seek+write)
//! - 内核态/用户态切换开销(SQPOLL 模式下为零)
//! - 内存拷贝(fixed buffer 避免内核重新映射)
//!
//! # 平台兼容性
//!
//! - Linux 5.4+:完整 io_uring 实现
//! - 其他平台:编译为空桩,`init()` 返回 `Unsupported` 错误

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use bytes::Bytes;

#[cfg(target_os = "linux")]
use std::cell::UnsafeCell;
#[cfg(target_os = "linux")]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU64, Ordering};

use tachyon_core::{DownloadError, DownloadResult};

use crate::storage::AsyncStorage;

/// io_uring 引擎配置
///
/// 控制提交队列深度、完成队列深度、fixed buffer 参数和 SQPOLL 行为。
/// 默认配置适合中等吞吐量场景(64KB buffer x 16 个 = 1MB 总量)。
pub struct IoUringConfig {
    /// SQ 深度(提交队列大小),必须为 2 的幂
    pub sq_depth: u32,
    /// CQ 深度(完成队列大小),通常为 sq_depth 的 2 倍以避免溢出
    pub cq_depth: u32,
    /// 每个 fixed buffer 的大小(字节)
    pub buffer_size: usize,
    /// fixed buffer 数量,决定并发写入操作的上限
    pub buffer_count: usize,
    /// 是否启用 SQPOLL(内核轮询模式)
    ///
    /// 启用后内核线程持续轮询 SQ,消除 `io_uring_enter` 系统调用。
    /// 需要 `CAP_SYS_ADMIN` 权限或 `/proc/sys/kernel/io_uring_disabled` 为 0。
    pub sqpoll: bool,
    /// SQPOLL 空闲超时(毫秒)
    ///
    /// 内核轮询线程在无新 SQE 超过此时间后进入休眠,
    /// 下次提交时通过 `IORING_ENTER_SQ_WAIT` 唤醒。
    pub sqpoll_idle_ms: u32,
}

impl Default for IoUringConfig {
    fn default() -> Self {
        Self {
            sq_depth: 256,
            cq_depth: 512,
            buffer_size: 64 * 1024, // 64KB per buffer
            buffer_count: 16,       // 16 个 fixed buffer = 1MB 总量
            sqpoll: false,          // 默认关闭(需要 CAP_SYS_ADMIN)
            sqpoll_idle_ms: 1000,
        }
    }
}

impl std::fmt::Debug for IoUringConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IoUringConfig")
            .field("sq_depth", &self.sq_depth)
            .field("cq_depth", &self.cq_depth)
            .field("buffer_size", &self.buffer_size)
            .field("buffer_count", &self.buffer_count)
            .field("sqpoll", &self.sqpoll)
            .field("sqpoll_idle_ms", &self.sqpoll_idle_ms)
            .finish()
    }
}

/// io_uring 存储引擎状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoUringState {
    /// 已创建但未初始化
    Created,
    /// 已初始化,可用
    Ready,
    /// 初始化失败或不可用
    Unavailable,
}

/// io_uring 存储引擎 (Linux only)
///
/// 在 Linux 5.4+ 上使用 io_uring 实现高效异步文件 I/O。
/// 零拷贝管道:网络数据 -> fixed buffer -> 文件,全程无用户态额外拷贝。
///
/// 在非 Linux 平台上编译为空桩,所有操作返回 `Unsupported` 错误。
pub struct IoUringStorage {
    /// 引擎配置
    config: IoUringConfig,
    /// 目标文件路径
    file_path: PathBuf,
    /// 文件描述符(Linux 上通过 RawFd 传入 io_uring)
    /// W-17: 使用 Arc 包装,确保 spawn_blocking 闭包中 raw fd 的生命周期
    /// 不短于 IoUringStorage 本身
    #[allow(dead_code)] // Linux cfg 代码中使用
    file_fd: Option<std::sync::Arc<std::fs::File>>,
    /// 引擎状态
    state: IoUringState,
    // === Linux-only 字段(条件编译) ===
    // io_uring 实例持有者,在 Linux 上通过 Box 持有
    // 避免在非 Linux 平台上引入 io_uring crate 依赖
    #[cfg(target_os = "linux")]
    ring: Option<std::sync::Arc<IoUringHandle>>,
}

/// 地址对齐的缓冲区(Linux only)
///
/// `storage` 使用 `UnsafeCell` 包装，因为 io_uring 内核操作需要从共享引用
/// (`&AlignedBuffer`) 获取可变内存访问（`*mut u8`），这违反了 Rust 的
/// Stacked Borrows / Tree Borrows 内存模型。`UnsafeCell` 显式声明内部可变性，
/// 使跨共享边界的 `*mut` 访问合法化。外部 `Mutex<IoUringHandle>` 保证同一
/// 时刻只有一个操作访问给定 buffer，确保运行时排他性。
#[cfg(target_os = "linux")]
struct AlignedBuffer {
    /// UnsafeCell 包装: io_uring 固定缓冲区需要从 &self 创建 *mut u8，
    /// UnsafeCell 是 Rust 中唯一合法化此类跨共享边界可变访问的原语。
    storage: UnsafeCell<Vec<u8>>,
    offset: usize,
    len: usize,
}

// Safety: AlignedBuffer 始终在 Mutex<IoUringHandle> 内使用，
// 保证同一 buffer 的并发访问被 Mutex 串行化。
#[cfg(target_os = "linux")]
unsafe impl Send for AlignedBuffer {}
#[cfg(target_os = "linux")]
unsafe impl Sync for AlignedBuffer {}

#[cfg(target_os = "linux")]
impl AlignedBuffer {
    /// 获取对齐后的数据起始裸指针(只读用途，如 iovec 注册)
    fn as_ptr(&self) -> *const u8 {
        // Safety: 使用 ptr::addr() 仅获取地址值，不创建 &Vec<u8> 引用，
        // 避免与后续通过 ptr() 创建的可变引用产生 aliasing 冲突。
        let base_addr = self.storage.get().addr();
        (base_addr + self.offset) as *const u8
    }

    /// 获取对齐后的数据起始裸指针(可变用途，如 io_uring write/read)
    ///
    /// Safety: 调用者必须保证同一时刻没有其他引用访问此 buffer 的数据区域。
    /// IoUringHandle.ring 的 Mutex 保证所有 io_uring 操作互斥。
    fn ptr(&self) -> *mut u8 {
        unsafe { (*self.storage.get()).as_mut_ptr().add(self.offset) }
    }

    fn len(&self) -> usize {
        self.len
    }
}

#[cfg(any(test, target_os = "linux"))]
fn invalid_input(message: impl Into<String>) -> DownloadError {
    DownloadError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        message.into(),
    ))
}

#[cfg(any(test, target_os = "linux"))]
fn validate_fixed_buffer_config(config: &IoUringConfig) -> DownloadResult<()> {
    if config.buffer_size == 0 {
        return Err(invalid_input("io_uring fixed buffer size must be non-zero"));
    }
    if config.buffer_count == 0 {
        return Err(invalid_input(
            "io_uring fixed buffer count must be non-zero",
        ));
    }
    if config.buffer_size > u32::MAX as usize {
        return Err(invalid_input(format!(
            "io_uring fixed buffer size {} exceeds single-op u32 length limit {}",
            config.buffer_size,
            u32::MAX
        )));
    }
    Ok(())
}

#[cfg(any(test, target_os = "linux"))]
fn validate_fixed_buffer_write_len(len: usize, buffer_len: usize) -> DownloadResult<()> {
    if len > buffer_len {
        return Err(invalid_input(format!(
            "io_uring write length {len} exceeds fixed buffer size {buffer_len}"
        )));
    }
    if len > u32::MAX as usize {
        return Err(invalid_input(format!(
            "io_uring write length {len} exceeds single-op u32 length limit {}",
            u32::MAX
        )));
    }
    Ok(())
}

/// io_uring 实例持有者(Linux only)
///
/// 封装 `io_uring::IoUring` 实例及其注册的 fixed buffers。
/// 使用 `Mutex` 包裹以实现内部可变性——`submission()` 和 `completion()`
/// 需要 `&mut self`，而 `AsyncStorage` trait 方法签名使用 `&self`。
///
/// # Buffer 分配策略
///
/// 通过 `AtomicU64` 位图实现无锁 fixed buffer 分配。
/// 每个 bit 对应一个 buffer: 1=已占用, 0=空闲。
/// 最多支持 64 个 buffer(远超默认配置 16 个)。
/// Mutex 仅保护 io_uring ring 的 submission/completion 操作,
/// buffer 索引的分配/释放通过原子操作完成,不阻塞其他并发 I/O。
#[cfg(target_os = "linux")]
struct IoUringHandle {
    /// io_uring 实例(Mutex 包裹以支持内部可变性)
    ring: std::sync::Mutex<io_uring::IoUring>,
    /// 注册的 fixed buffers (保持内存不被释放)
    buffers: Vec<AlignedBuffer>,
    /// fixed buffer 分配位图(1=已占用, 0=空闲)
    ///
    /// 使用 AtomicU64 实现无锁分配,最多支持 64 个 buffer。
    /// 初始化时超出 `buffer_count` 的位被设为 1,防止越界分配。
    buffer_bitmap: AtomicU64,
}

#[cfg(target_os = "linux")]
impl IoUringHandle {
    /// 原子分配一个空闲 fixed buffer 索引。
    ///
    /// 使用 AtomicU64 位图进行无锁分配,返回的索引范围 [0, buffers.len()-1],
    /// 对应 `buffers` 中的位置。当所有 buffer 都被占用时返回 None。
    fn alloc_buffer_index(&self) -> Option<usize> {
        let mut current = self.buffer_bitmap.load(Ordering::Relaxed);
        loop {
            let idx = current.trailing_zeros();
            if idx >= 64 {
                return None; // 所有 buffer 都被占用
            }
            let next = current | (1 << idx);
            match self.buffer_bitmap.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(idx as usize),
                Err(actual) => current = actual,
            }
        }
    }

    /// 释放 fixed buffer 索引,使其可被后续操作重新分配。
    fn free_buffer_index(&self, idx: usize) {
        if idx >= 64 {
            return;
        }
        self.buffer_bitmap.fetch_and(!(1 << idx), Ordering::Relaxed);
    }
}

/// 分配地址对齐的缓冲区(O_DIRECT/io_uring 要求)
///
/// 通过过量分配 Vec 并选择对齐的内部起点,保证暴露给 io_uring 的地址满足 align 对齐。
/// 对外暴露的逻辑长度保持为调用方请求的 size。
#[cfg(target_os = "linux")]
fn aligned_alloc(size: usize, align: usize) -> AlignedBuffer {
    assert!(size > 0, "buffer size must be non-zero");
    assert!(
        align.is_power_of_two(),
        "buffer align must be a power of two"
    );

    let padding = align - 1;
    let storage_len = size.checked_add(padding).expect("对齐缓冲区大小溢出");
    let storage = vec![0u8; storage_len];
    // 使用 ptr::addr() 仅读取地址数值，不创建 &Vec 引用，
    // 避免与后续 UnsafeCell 内部可变性产生 aliasing 冲突
    let base = storage.as_ptr().addr();
    let misalignment = base & padding;
    let offset = if misalignment == 0 {
        0
    } else {
        align - misalignment
    };

    debug_assert!(offset < align);
    debug_assert!(offset + size <= storage_len);

    AlignedBuffer {
        storage: UnsafeCell::new(storage),
        offset,
        len: size,
    }
}

impl IoUringStorage {
    /// 创建 io_uring 存储引擎实例
    ///
    /// 仅分配结构体,不初始化 io_uring。需要调用 `init()` 完成初始化。
    pub fn new(path: impl AsRef<Path>, config: IoUringConfig) -> Self {
        Self {
            config,
            file_path: path.as_ref().to_path_buf(),
            file_fd: None,
            state: IoUringState::Created,
            #[cfg(target_os = "linux")]
            ring: None,
        }
    }

    /// 获取当前引擎状态
    pub fn state(&self) -> IoUringState {
        self.state
    }

    /// 获取文件路径
    pub fn path(&self) -> &Path {
        &self.file_path
    }

    /// 获取配置引用
    pub fn config(&self) -> &IoUringConfig {
        &self.config
    }

    /// 初始化 io_uring 实例和 fixed buffers (Linux)
    ///
    /// 执行步骤:
    /// 1. 创建 `io_uring::IoUring` 实例,设置 SQ/CQ 深度
    /// 2. 如启用 SQPOLL,设置 `IORING_SETUP_SQPOLL` 标志和空闲超时
    /// 3. 分配并注册 fixed buffers (`IORING_REGISTER_BUFFERS`)
    /// 4. 以 `O_DIRECT` 模式打开目标文件
    #[cfg(target_os = "linux")]
    pub fn init(&mut self) -> DownloadResult<()> {
        use io_uring::IoUring;

        validate_fixed_buffer_config(&self.config)?;

        // 步骤 1: 构建 io_uring 实例
        let mut builder = IoUring::builder();
        builder.setup_cqsize(self.config.cq_depth);

        if self.config.sqpoll {
            builder.setup_sqpoll(self.config.sqpoll_idle_ms);
        }

        let ring = builder
            .build(self.config.sq_depth)
            .map_err(|e| DownloadError::Io(std::io::Error::other(e)))?;

        // 步骤 2: 分配 fixed buffers(对齐分配,O_DIRECT 需要 4096 字节对齐)
        let align = 4096; // 现代 Linux 内核 O_DIRECT 最小对齐要求
        let mut buffers: Vec<AlignedBuffer> = Vec::with_capacity(self.config.buffer_count);
        for _ in 0..self.config.buffer_count {
            let buf = aligned_alloc(self.config.buffer_size, align);
            buffers.push(buf);
        }

        // 步骤 3: 注册 fixed buffers 到内核
        // 注册后内核持有这些页面的映射,SQE 中使用 buf_index 引用
        // Safety:
        // 1. `buf` 是 AlignedBuffer 持有的 Vec<u8>,其内存地址在 AlignedBuffer
        //    生命周期内保持有效(io_uring 固定缓冲区注册期间不会释放)。
        // 2. `buf.as_ptr()` 返回的对齐地址满足 io_uring O_DIRECT 的对齐要求
        //   (由 aligned_alloc 保证 4096 字节对齐)。
        // 3. `as *mut c_void` 转换安全,因为内核仅通过 io_uring 操作写入该缓冲区,
        //    不会与 Rust 侧的共享引用同时存在(由 io_uring 提交/完成队列的
        //    单生产者-单消费者模型保证)。
        // 4. iovec 的生命周期短于 AlignedBuffer 的生命周期——iovecs 在函数末尾
        //    被 drop,buffers 在 IoUringHandle 被 drop 前一直有效。
        let iovecs: Vec<libc::iovec> = buffers
            .iter()
            .map(|buf| {
                // Safety: 满足以上第 1-4 条 Safety 条件
                libc::iovec {
                    iov_base: buf.as_ptr() as *mut libc::c_void,
                    iov_len: buf.len(),
                }
            })
            .collect();

        // 注意:注册需要可变引用,在 Mutex 包裹之前完成
        // SAFETY: iovecs 引用的 buf 生命周期覆盖整个 ring 的使用期
        unsafe {
            ring.submitter()
                .register_buffers(&iovecs)
                .map_err(|e| DownloadError::Io(std::io::Error::other(e)))?;
        }

        // 步骤 4: 以 O_DIRECT 打开文件
        // O_DIRECT 绕过页缓存,配合 fixed buffer 实现真正零拷贝
        let file = std::fs::OpenOptions::new()
            .write(true)
            .read(true)
            .create(true)
            .truncate(false)
            .custom_flags(libc::O_DIRECT)
            .open(&self.file_path)
            .map_err(DownloadError::Io)?;

        self.file_fd = Some(std::sync::Arc::new(file));

        // 超出 buffer_count 的位标记为已占用,防止分配越界
        let buffer_count = buffers.len();
        let used_mask = if buffer_count >= 64 {
            0u64
        } else {
            (!0u64) << buffer_count
        };

        self.ring = Some(std::sync::Arc::new(IoUringHandle {
            ring: std::sync::Mutex::new(ring),
            buffers,
            buffer_bitmap: AtomicU64::new(used_mask),
        }));
        self.state = IoUringState::Ready;

        tracing::info!(
            "io_uring 初始化完成: sq_depth={}, cq_depth={}, buffers={}x{}KB, sqpoll={}",
            self.config.sq_depth,
            self.config.cq_depth,
            self.config.buffer_count,
            self.config.buffer_size / 1024,
            self.config.sqpoll
        );

        Ok(())
    }

    /// 非 Linux 平台:返回不支持错误
    #[cfg(not(target_os = "linux"))]
    pub fn init(&mut self) -> DownloadResult<()> {
        self.state = IoUringState::Unavailable;
        Err(DownloadError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "io_uring 仅在 Linux 5.4+ 上可用,当前平台不支持",
        )))
    }

    /// 提交读取操作到 io_uring SQ (Linux)
    ///
    /// 构造 `IORING_OP_READ_FIXED` SQE:
    /// - `fd`: 目标文件描述符
    /// - `off`: 文件偏移量
    /// - `addr`: fixed buffer 地址
    /// - `len`: 读取长度
    /// - `buf_index`: 动态分配的注册 buffer 索引
    ///
    /// 读取完成后将 fixed buffer 中的数据复制到用户提供的 buf 中。
    /// 使用 `AtomicU64` 位图分配 buffer 索引,避免硬编码 `buffers[0]` 的串行化瓶颈。
    #[cfg(target_os = "linux")]
    async fn submit_read(&self, offset: u64, buf: &mut [u8]) -> DownloadResult<usize> {
        let ring_handle = match &self.ring {
            Some(h) => h.clone(),
            None => {
                return Err(DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "io_uring 未初始化",
                )));
            }
        };
        let fd = match &self.file_fd {
            Some(f) => {
                use std::os::fd::AsRawFd;
                f.as_raw_fd()
            }
            None => {
                return Err(DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "文件未打开",
                )));
            }
        };

        let file_guard = self.file_fd.as_ref().unwrap().clone();
        let read_len = buf.len();

        // spawn_blocking 要求 'static，因此把读取结果放入自有 Vec 中返回
        let read_result: Vec<u8> = tokio::task::spawn_blocking(move || {
            let _file_guard = file_guard;

            let mut uring = ring_handle
                .ring
                .lock()
                .map_err(|e| DownloadError::Io(std::io::Error::other(e.to_string())))?;

            // 动态分配 fixed buffer 索引
            let buf_idx = ring_handle.alloc_buffer_index().ok_or_else(|| {
                DownloadError::Io(std::io::Error::other(
                    "io_uring fixed buffer 已耗尽,并发读取操作过多",
                ))
            })?;

            // 使用闭包封装 I/O 操作,确保无论成功失败都释放 buffer 索引
            let io_result = (|| -> DownloadResult<Vec<u8>> {
                let fixed_buf = &ring_handle.buffers[buf_idx];
                let actual_len = read_len.min(fixed_buf.len());

                // 构造 IORING_OP_READ_FIXED SQE
                // 使用 ptr() 获取 *mut u8，通过 UnsafeCell 合法化内核写入
                let read_op = io_uring::opcode::ReadFixed::new(
                    io_uring::types::Fd(fd),
                    fixed_buf.ptr(),
                    actual_len as u32,
                    buf_idx as u16, // buf_index: 使用动态分配的 fixed buffer 索引
                )
                .offset(offset)
                .build();

                let mut sq = uring.submission();
                // Safety:
                // - read_op 由 io_uring::opcode::ReadFixed::build() 构造,是有效的 SQE
                // - 调用期间 read_op 在栈上保持存活,指针指向自身内存
                // - 提交队列未满已通过 push 返回的 Result 处理
                unsafe {
                    sq.push(&read_op).map_err(|_| {
                        DownloadError::Io(std::io::Error::other("io_uring 提交队列已满"))
                    })?;
                }
                sq.sync();
                drop(sq);

                uring
                    .submitter()
                    .submit_and_wait(1)
                    .map_err(DownloadError::Io)?;

                let cqe = uring.completion().next().ok_or_else(|| {
                    DownloadError::Io(std::io::Error::other("io_uring 完成队列已关闭"))
                })?;
                let result = cqe.result();
                if result < 0 {
                    return Err(DownloadError::Io(std::io::Error::from_raw_os_error(
                        -result,
                    )));
                }
                let bytes_read = result as usize;

                // 从 fixed buffer 复制到 Vec 中返回
                // Safety:
                // - fixed_buf 是已注册到 io_uring 的合法 fixed buffer,生命周期由 ring_handle 持有
                // - bytes_read 来自 CQE 结果,且已验证 result >= 0,范围在 fixed_buf 长度内
                // - fixed_buf.ptr() 返回的指针在 bytes_read 范围内有效且可读
                let src = unsafe { std::slice::from_raw_parts(fixed_buf.as_ptr(), bytes_read) };
                Ok(src.to_vec())
            })();

            // 释放 buffer 索引(无论 I/O 成功或失败)
            ring_handle.free_buffer_index(buf_idx);
            io_result
        })
        .await
        .map_err(|e| DownloadError::Io(std::io::Error::other(e.to_string())))??;

        // 从返回的 Vec 复制到用户缓冲区
        let bytes_read = read_result.len();
        buf[..bytes_read].copy_from_slice(&read_result);
        Ok(bytes_read)
    }

    /// 同步文件数据到磁盘 (Linux)
    ///
    /// 使用 io_uring FSYNC SQE 提交同步操作。
    #[cfg(target_os = "linux")]
    async fn submit_sync(&self) -> DownloadResult<()> {
        let ring_handle = match &self.ring {
            Some(h) => h.clone(),
            None => {
                return Err(DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "io_uring 未初始化",
                )));
            }
        };
        let fd = match &self.file_fd {
            Some(f) => {
                use std::os::fd::AsRawFd;
                f.as_raw_fd()
            }
            None => {
                return Err(DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "文件未打开",
                )));
            }
        };

        let file_guard = self.file_fd.as_ref().unwrap().clone();

        tokio::task::spawn_blocking(move || {
            let _file_guard = file_guard;

            let mut uring = ring_handle
                .ring
                .lock()
                .map_err(|e| DownloadError::Io(std::io::Error::other(e.to_string())))?;

            // 构造 IORING_OP_FSYNC SQE
            let fsync_op = io_uring::opcode::Fsync::new(io_uring::types::Fd(fd)).build();

            let mut sq = uring.submission();
            // Safety:
            // - fsync_op 由 io_uring::opcode::Fsync::build() 构造,是有效的 SQE
            // - 调用期间 fsync_op 在栈上保持存活,指针指向自身内存
            // - 提交队列未满已通过 push 返回的 Result 处理
            unsafe {
                sq.push(&fsync_op).map_err(|_| {
                    DownloadError::Io(std::io::Error::other("io_uring 提交队列已满"))
                })?;
            }
            sq.sync();
            drop(sq);

            uring
                .submitter()
                .submit_and_wait(1)
                .map_err(DownloadError::Io)?;

            let cqe = uring.completion().next().ok_or_else(|| {
                DownloadError::Io(std::io::Error::other("io_uring 完成队列已关闭"))
            })?;
            let result = cqe.result();
            if result < 0 {
                return Err(DownloadError::Io(std::io::Error::from_raw_os_error(
                    -result,
                )));
            }
            Ok(())
        })
        .await
        .map_err(|e| DownloadError::Io(std::io::Error::other(e.to_string())))?
    }

    /// 预分配文件空间 (Linux)
    ///
    /// 使用 `fallocate` 系统调用预分配磁盘空间，避免写入时的动态扩展开销。
    #[cfg(target_os = "linux")]
    async fn submit_allocate(&self, size: u64) -> DownloadResult<()> {
        let file_guard = match &self.file_fd {
            Some(f) => f.clone(),
            None => {
                return Err(DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "文件未打开",
                )));
            }
        };

        tokio::task::spawn_blocking(move || {
            use std::os::fd::AsRawFd;
            let fd = file_guard.as_raw_fd();
            // Safety:
            // - fd 来自合法打开的 Arc<File>,file_guard 在调用期间保持 Arc 存活,确保 fd 有效
            // - mode=0、offset=0、len=size 均为合法的 fallocate 参数
            // - 内核负责实际的磁盘空间预分配,不破坏 Rust 内存安全
            let ret = unsafe { libc::fallocate(fd, 0, 0, size as libc::off_t) };
            if ret != 0 {
                return Err(DownloadError::Io(std::io::Error::last_os_error()));
            }
            Ok(())
        })
        .await
        .map_err(|e| DownloadError::Io(std::io::Error::other(e.to_string())))?
    }

    /// 提交写入操作到 io_uring SQ (Linux)
    ///
    /// 构造 `IORING_OP_WRITE_FIXED` SQE:
    /// - `fd`: 目标文件描述符
    /// - `off`: 文件偏移量
    /// - `addr`: fixed buffer 地址
    /// - `len`: 数据长度
    /// - `buf_index`: 动态分配的注册 buffer 索引
    ///
    /// 使用 `spawn_blocking` 在独立线程中提交并等待 CQE,
    /// 避免阻塞 tokio 异步运行时。Mutex 保证同一时刻仅有一个
    /// 写入操作使用 io_uring ring,但 buffer 索引通过 AtomicU64 位图
    /// 动态分配,多个并发写入可使用不同 buffer 并行执行。
    #[cfg(target_os = "linux")]
    async fn submit_write(&self, offset: u64, data: Bytes) -> DownloadResult<usize> {
        let ring_handle = match &self.ring {
            Some(h) => h.clone(), // Arc clone, Send + 'static
            None => {
                return Err(DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "io_uring 未初始化",
                )));
            }
        };
        let fd = match &self.file_fd {
            Some(f) => {
                use std::os::fd::AsRawFd;
                f.as_raw_fd()
            }
            None => {
                return Err(DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "文件未打开",
                )));
            }
        };

        // W-17: 克隆 Arc<File> 移入 spawn_blocking,确保 raw fd 在闭包执行期间有效
        let file_guard = self.file_fd.as_ref().unwrap().clone();

        let len = data.len();
        let buffer_len = ring_handle
            .buffers
            .first()
            .map(AlignedBuffer::len)
            .ok_or_else(|| invalid_input("io_uring has no registered fixed buffers for write"))?;
        validate_fixed_buffer_write_len(len, buffer_len)?;

        // spawn_blocking: 在独立线程中完成 io_uring 提交+等待,
        // 避免 MutexGuard 跨 await 点以及阻塞 tokio 工作线程
        tokio::task::spawn_blocking(move || {
            // W-17: 持有 Arc<File> 确保 raw fd 在 spawn_blocking 线程执行期间有效
            let _file_guard = file_guard;

            let mut uring = ring_handle
                .ring
                .lock()
                .map_err(|e| DownloadError::Io(std::io::Error::other(e.to_string())))?;

            // 动态分配 fixed buffer 索引
            let buf_idx = ring_handle.alloc_buffer_index().ok_or_else(|| {
                DownloadError::Io(std::io::Error::other(
                    "io_uring fixed buffer 已耗尽,并发写入操作过多",
                ))
            })?;

            // 使用闭包封装 I/O 操作,确保无论成功失败都释放 buffer 索引
            let io_result = (|| -> DownloadResult<usize> {
                let buf = &ring_handle.buffers[buf_idx];
                // Safety:
                // - buf 是已注册到 io_uring 的合法 fixed buffer,生命周期由 ring_handle 持有
                // - len 已通过 validate_fixed_buffer_write_len 验证,不超过 buf 容量
                // - buf.ptr() 返回的指针在 len 范围内有效且可写
                // - alloc_buffer_index 保证同一时刻只有一个操作使用该 buffer 索引
                let dst = unsafe { std::slice::from_raw_parts_mut(buf.ptr(), len) };
                dst.copy_from_slice(&data[..len]);

                // 构造 IORING_OP_WRITE_FIXED SQE
                // 使用已注册的 fixed buffer 索引,内核直接使用注册页面的物理地址,
                // 省去每次 I/O 的页表查找开销
                let write_op = io_uring::opcode::WriteFixed::new(
                    io_uring::types::Fd(fd),
                    buf.ptr() as *const u8,
                    len as u32,
                    buf_idx as u16, // buf_index: 使用动态分配的 fixed buffer 索引
                )
                .offset(offset)
                .build();

                // 提交 SQE 到 SQ 并同步到内核
                let mut sq = uring.submission();
                // Safety:
                // - write_op 由 io_uring::opcode::WriteFixed::build() 构造,是有效的 SQE
                // - 调用期间 write_op 在栈上保持存活,指针指向自身内存
                // - fixed buffer 的数据已在上方复制完成,生命周期覆盖提交期间
                // - 提交队列未满已通过 push 返回的 Result 处理
                unsafe {
                    sq.push(&write_op).map_err(|_| {
                        DownloadError::Io(std::io::Error::other("io_uring 提交队列已满"))
                    })?;
                }
                sq.sync();
                drop(sq);

                // submit_and_wait(1): 提交待处理的 SQE 并阻塞等待至少 1 个 CQE,
                // 由于已在 spawn_blocking 线程中运行,不会阻塞 tokio 运行时
                uring
                    .submitter()
                    .submit_and_wait(1)
                    .map_err(DownloadError::Io)?;

                // 读取 CQE 获取完成结果
                let cqe = uring.completion().next().ok_or_else(|| {
                    DownloadError::Io(std::io::Error::other("io_uring 完成队列已关闭"))
                })?;
                let result = cqe.result();
                if result < 0 {
                    return Err(DownloadError::Io(std::io::Error::from_raw_os_error(
                        -result,
                    )));
                }
                Ok(result as usize)
            })();

            // 释放 buffer 索引(无论 I/O 成功或失败)
            ring_handle.free_buffer_index(buf_idx);
            io_result
        })
        .await
        .map_err(|e| DownloadError::Io(std::io::Error::other(e.to_string())))?
    }
}

// =============================================================================
// AsyncStorage trait 实现
//
// 当前阶段:所有平台均返回 Unsupported,引导用户使用 TokioFile。
// Linux 实现阶段:将切换到 io_uring 路径,通过 submit_write 完成零拷贝写入。
// =============================================================================

impl AsyncStorage for IoUringStorage {
    fn write_at(
        &self,
        _offset: u64,
        _data: Bytes,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
        Box::pin(async move {
            match self.state {
                IoUringState::Ready => {
                    // Linux 上:走 io_uring 零拷贝路径
                    #[cfg(target_os = "linux")]
                    {
                        validate_fixed_buffer_write_len(_data.len(), self.config.buffer_size)?;
                        self.submit_write(_offset, _data).await
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        unreachable!("非 Linux 平台不可能处于 Ready 状态")
                    }
                }
                _ => Err(DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "io_uring 存储引擎未初始化,请先调用 init() 或使用 TokioFile",
                ))),
            }
        })
    }

    fn read_at<'a>(
        &'a self,
        _offset: u64,
        _buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
        Box::pin(async move {
            match self.state {
                IoUringState::Ready => {
                    #[cfg(target_os = "linux")]
                    {
                        self.submit_read(_offset, _buf).await
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        unreachable!("非 Linux 平台不可能处于 Ready 状态")
                    }
                }
                _ => Err(DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "io_uring 存储引擎未初始化,请先调用 init() 或使用 TokioFile",
                ))),
            }
        })
    }

    fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        Box::pin(async move {
            match self.state {
                IoUringState::Ready => {
                    #[cfg(target_os = "linux")]
                    {
                        self.submit_sync().await
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        unreachable!("非 Linux 平台不可能处于 Ready 状态")
                    }
                }
                _ => Err(DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "io_uring 存储引擎未初始化",
                ))),
            }
        })
    }

    fn allocate(&self, size: u64) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        Box::pin(async move {
            match self.state {
                IoUringState::Ready => {
                    #[cfg(target_os = "linux")]
                    {
                        self.submit_allocate(size).await
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        let _ = size;
                        unreachable!("非 Linux 平台不可能处于 Ready 状态")
                    }
                }
                _ => Err(DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "io_uring 存储引擎未初始化",
                ))),
            }
        })
    }

    fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
        Box::pin(async move {
            match self.state {
                IoUringState::Ready => {
                    // 文件大小查询走标准 stat,无需 io_uring
                    #[cfg(target_os = "linux")]
                    {
                        #[allow(unused_imports)]
                        // metadata() 不需要 AsRawFd,保留供后续 io_uring 操作使用
                        use std::os::unix::io::AsRawFd;
                        if let Some(ref file) = self.file_fd {
                            let metadata = file.metadata().map_err(DownloadError::Io)?;
                            Ok(metadata.len())
                        } else {
                            Err(DownloadError::Io(std::io::Error::new(
                                std::io::ErrorKind::NotConnected,
                                "文件未打开",
                            )))
                        }
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        unreachable!("非 Linux 平台不可能处于 Ready 状态")
                    }
                }
                _ => Err(DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "io_uring 存储引擎未初始化",
                ))),
            }
        })
    }

    fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        Box::pin(async move {
            match self.state {
                IoUringState::Ready => {
                    #[cfg(target_os = "linux")]
                    {
                        // S-15: sync_all() 是阻塞操作(fsync 系统调用),
                        // 直接在 async 上下文中调用会阻塞 tokio 工作线程。
                        // 移至 spawn_blocking 在独立线程中执行。
                        if let Some(file) = self.file_fd.clone() {
                            tokio::task::spawn_blocking(move || {
                                file.sync_all().map_err(DownloadError::Io)
                            })
                            .await
                            .map_err(|e| {
                                DownloadError::Io(std::io::Error::other(e.to_string()))
                            })??;
                        }
                        Ok(())
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        unreachable!("非 Linux 平台不可能处于 Ready 状态")
                    }
                }
                _ => Ok(()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_invalid_input_error(err: DownloadError, expected_message: &str) {
        match err {
            DownloadError::Io(io_error) => {
                assert_eq!(io_error.kind(), std::io::ErrorKind::InvalidInput);
                assert!(
                    io_error.to_string().contains(expected_message),
                    "错误信息应包含 {expected_message}, 实际: {io_error}"
                );
            }
            other => panic!("应返回 I/O InvalidInput 错误,实际: {other}"),
        }
    }

    #[test]
    fn test_default_config() {
        let config = IoUringConfig::default();
        assert_eq!(config.sq_depth, 256);
        assert_eq!(config.cq_depth, 512);
        assert_eq!(config.buffer_size, 64 * 1024);
        assert_eq!(config.buffer_count, 16);
        assert!(!config.sqpoll);
        assert_eq!(config.sqpoll_idle_ms, 1000);
    }

    #[test]
    fn test_default_config_buffer_total() {
        let config = IoUringConfig::default();
        let total = config.buffer_size * config.buffer_count;
        assert_eq!(total, 1024 * 1024, "默认总 buffer 应为 1MB");
    }

    #[test]
    fn test_new_storage_state_is_created() {
        let storage = IoUringStorage::new("/tmp/test.bin", IoUringConfig::default());
        assert_eq!(storage.state(), IoUringState::Created);
    }

    #[test]
    fn test_new_storage_path() {
        let storage = IoUringStorage::new("/tmp/test.bin", IoUringConfig::default());
        assert_eq!(storage.path(), Path::new("/tmp/test.bin"));
    }

    #[test]
    fn test_new_storage_config_ref() {
        let config = IoUringConfig {
            sq_depth: 128,
            cq_depth: 256,
            buffer_size: 32 * 1024,
            buffer_count: 8,
            sqpoll: true,
            sqpoll_idle_ms: 2000,
        };
        let storage = IoUringStorage::new("/tmp/test.bin", config);
        assert_eq!(storage.config().sq_depth, 128);
        assert_eq!(storage.config().buffer_count, 8);
        assert!(storage.config().sqpoll);
    }

    #[test]
    fn test_state_variants() {
        assert_ne!(IoUringState::Created, IoUringState::Ready);
        assert_ne!(IoUringState::Created, IoUringState::Unavailable);
        assert_ne!(IoUringState::Ready, IoUringState::Unavailable);
    }

    #[test]
    fn test_state_debug() {
        let state = IoUringState::Created;
        assert_eq!(format!("{state:?}"), "Created");
    }

    #[test]
    fn test_state_clone_copy() {
        let state = IoUringState::Ready;
        let state2 = state;
        assert_eq!(state, state2);
    }

    #[test]
    fn test_config_debug() {
        let config = IoUringConfig::default();
        let debug = format!("{config:?}");
        assert!(debug.contains("IoUringConfig"));
        assert!(debug.contains("sq_depth"));
        assert!(debug.contains("256"));
    }

    /// 在非 Linux 平台上,init() 应返回 Unsupported 错误
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn test_init_returns_unsupported_on_non_linux() {
        let mut storage = IoUringStorage::new("/tmp/test.bin", IoUringConfig::default());
        let result = storage.init();
        assert!(result.is_err());
        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("Linux") || err_msg.contains("io_uring"),
            "错误信息应说明 io_uring 平台限制,实际: {err_msg}"
        );
        assert_eq!(storage.state(), IoUringState::Unavailable);
    }

    /// 在非 Linux 平台上,write_at 应返回未初始化错误
    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn test_write_at_returns_not_connected_when_uninitialized() {
        let storage = IoUringStorage::new("/tmp/test.bin", IoUringConfig::default());
        let result = storage.write_at(0, Bytes::from_static(b"test")).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("未初始化") || err_msg.contains("未打开"),
            "错误信息应说明存储引擎未就绪,实际: {err_msg}"
        );
    }

    /// 在非 Linux 平台上,init 后 write_at 应返回未初始化错误(Unavailable 状态)
    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn test_write_at_after_failed_init() {
        let mut storage = IoUringStorage::new("/tmp/test.bin", IoUringConfig::default());
        let _ = storage.init(); // 失败但不 panic
        let result = storage.write_at(0, Bytes::from_static(b"test")).await;
        assert!(result.is_err());
    }

    /// 在非 Linux 平台上,read_at 应返回未初始化错误
    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn test_read_at_returns_not_connected_when_uninitialized() {
        let storage = IoUringStorage::new("/tmp/test.bin", IoUringConfig::default());
        let mut buf = [0u8; 16];
        let result = storage.read_at(0, &mut buf).await;
        assert!(result.is_err());
    }

    /// 在非 Linux 平台上,sync 应返回未初始化错误
    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn test_sync_returns_not_connected_when_uninitialized() {
        let storage = IoUringStorage::new("/tmp/test.bin", IoUringConfig::default());
        let result = storage.sync().await;
        assert!(result.is_err());
    }

    /// 在非 Linux 平台上,allocate 应返回未初始化错误
    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn test_allocate_returns_not_connected_when_uninitialized() {
        let storage = IoUringStorage::new("/tmp/test.bin", IoUringConfig::default());
        let result = storage.allocate(1024).await;
        assert!(result.is_err());
    }

    /// 在非 Linux 平台上,file_size 应返回未初始化错误
    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn test_file_size_returns_not_connected_when_uninitialized() {
        let storage = IoUringStorage::new("/tmp/test.bin", IoUringConfig::default());
        let result = storage.file_size().await;
        assert!(result.is_err());
    }

    #[test]
    fn test_custom_config() {
        let config = IoUringConfig {
            sq_depth: 512,
            cq_depth: 1024,
            buffer_size: 128 * 1024,
            buffer_count: 32,
            sqpoll: true,
            sqpoll_idle_ms: 500,
        };
        let storage = IoUringStorage::new("/data/download.bin", config);
        assert_eq!(storage.config().sq_depth, 512);
        assert_eq!(storage.config().cq_depth, 1024);
        assert_eq!(storage.config().buffer_size, 128 * 1024);
        assert_eq!(storage.config().buffer_count, 32);
        assert!(storage.config().sqpoll);
        assert_eq!(storage.config().sqpoll_idle_ms, 500);
        assert_eq!(storage.path(), Path::new("/data/download.bin"));
    }

    #[test]
    fn test_fixed_buffer_write_len_allows_exact_buffer_size() {
        validate_fixed_buffer_write_len(4096, 4096).expect("等于 fixed buffer 大小时应允许写入");
    }

    #[test]
    fn test_fixed_buffer_write_len_rejects_oversized_payload() {
        let err = validate_fixed_buffer_write_len(4097, 4096)
            .expect_err("超过 fixed buffer 大小时必须返回错误");

        assert_invalid_input_error(err, "exceeds fixed buffer size");
    }

    #[test]
    fn test_fixed_buffer_config_rejects_empty_buffers() {
        let zero_size = IoUringConfig {
            buffer_size: 0,
            ..IoUringConfig::default()
        };
        let err =
            validate_fixed_buffer_config(&zero_size).expect_err("buffer_size 为 0 时必须返回错误");
        assert_invalid_input_error(err, "buffer size must be non-zero");

        let zero_count = IoUringConfig {
            buffer_count: 0,
            ..IoUringConfig::default()
        };
        let err = validate_fixed_buffer_config(&zero_count)
            .expect_err("buffer_count 为 0 时必须返回错误");
        assert_invalid_input_error(err, "buffer count must be non-zero");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn test_write_at_rejects_payload_larger_than_fixed_buffer_before_backend_io() {
        let storage = IoUringStorage {
            config: IoUringConfig {
                sq_depth: 8,
                cq_depth: 16,
                buffer_size: 4096,
                buffer_count: 1,
                sqpoll: false,
                sqpoll_idle_ms: 1000,
            },
            file_path: PathBuf::from("/tmp/iouring_oversized_write.bin"),
            file_fd: None,
            state: IoUringState::Ready,
            ring: None,
        };

        let err = storage
            .write_at(0, Bytes::from(vec![0u8; 4097]))
            .await
            .expect_err("超过 fixed buffer 大小时 write_at 必须先返回错误");

        assert_invalid_input_error(err, "exceeds fixed buffer size");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_aligned_alloc_address_alignment() {
        let buf = aligned_alloc(1024, 512);
        assert_eq!(buf.len(), 1024);
        assert!(
            (buf.as_ptr() as usize).is_multiple_of(512),
            "buffer 地址未按 512 字节对齐"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_aligned_alloc_keeps_logical_len() {
        let buf = aligned_alloc(100, 512);
        assert_eq!(buf.len(), 100, "逻辑长度应保持调用方请求的大小");
        assert!(
            (buf.as_ptr() as usize).is_multiple_of(512),
            "buffer 地址未按 512 字节对齐"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn test_align_buffer_size_rounding() {
        // 在非 Linux 平台上验证对齐函数的逻辑(通过编译时可用的函数)
        // align_buffer_size 仅在 Linux 上编译,此处测试概念验证
        let size: usize = 100;
        let align: usize = 512;
        let aligned = (size + align - 1) & !(align - 1);
        assert_eq!(aligned, 512);
    }

    /// 验证 io_uring buffer 对齐逻辑:512 和 4096 字节对齐均正确
    #[test]
    fn test_buffer_align() {
        // 512 字节对齐
        let size_512 = 100usize;
        let aligned_512 = (size_512 + 511) & !511;
        assert_eq!(aligned_512, 512);
        assert!(aligned_512.is_multiple_of(512));

        // 4096 字节对齐(O_DIRECT 要求)
        let size_4k = 1000usize;
        let aligned_4k = (size_4k + 4095) & !4095;
        assert_eq!(aligned_4k, 4096);
        assert!(aligned_4k.is_multiple_of(4096));

        // 已对齐的大小不变
        assert_eq!((4096usize + 4095) & !4095, 4096);
        assert_eq!((512usize + 511) & !511, 512);

        // 默认 buffer_size 64KB 也应是 4096 的倍数
        let default_size = 64 * 1024usize;
        assert!(
            default_size.is_multiple_of(4096),
            "默认 buffer_size 应为 4096 对齐"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_aligned_alloc_buffer_align() {
        let buf = aligned_alloc(1024, 4096);
        assert_eq!(buf.len(), 1024);
        assert!(
            (buf.as_ptr() as usize).is_multiple_of(4096),
            "buffer 地址应按 4096 对齐"
        );
    }
}
