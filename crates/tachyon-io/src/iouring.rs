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

use bytes::{Bytes, BytesMut};

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
/// 使跨共享边界的 `*mut` 访问合法化。外部 driver task 架构保证同一
/// 时刻只有一个操作访问给定 buffer，确保运行时排他性。
#[cfg(target_os = "linux")]
struct AlignedBuffer {
    /// UnsafeCell 包装: io_uring 固定缓冲区需要从 &self 创建 *mut u8，
    /// UnsafeCell 是 Rust 中唯一合法化此类跨共享边界可变访问的原语。
    storage: UnsafeCell<Vec<u8>>,
    offset: usize,
    len: usize,
}

// Safety: AlignedBuffer 始终在 driver task 架构下使用，
// 保证同一 buffer 的并发访问被 driver task 串行化。
#[cfg(target_os = "linux")]
unsafe impl Send for AlignedBuffer {}
#[cfg(target_os = "linux")]
unsafe impl Sync for AlignedBuffer {}

#[cfg(target_os = "linux")]
impl AlignedBuffer {
    /// 获取对齐后的数据起始裸指针(只读用途，如 iovec 注册)
    fn as_ptr(&self) -> *const u8 {
        // 复用 ptr() 保证与可变指针指向同一地址；as_mut_ptr() 通过 UnsafeCell
        // 内部的原始 Vec 获取堆数据指针，加上创建时记录的 offset 即为对齐地址。
        self.ptr() as *const u8
    }

    /// 获取对齐后的数据起始裸指针(可变用途，如 io_uring write/read)
    ///
    /// Safety: 调用者必须保证同一时刻没有其他引用访问此 buffer 的数据区域。
    /// IoUringHandle.ring 的 Mutex 保证所有 io_uring 操作互斥。
    fn ptr(&self) -> *mut u8 {
        // SAFETY: self.storage 是 UnsafeCell<Vec<u8>>,通过 get() 获取 *mut Vec<u8>
        // 是 UnsafeCell 合法化的内部可变性模式。解引用得到 &mut Vec<u8>,
        // as_mut_ptr() 返回堆数据指针,加上 offset 是对齐偏移(由
        // aligned_alloc 计算保证 offset < align 且 offset + len <= storage_len)。
        // 外部 Mutex 保证运行时排他性,不会与其它引用产生别名冲突。
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

/// O_DIRECT 最小对齐要求(字节)。
///
/// Linux 内核对 O_DIRECT 的通用要求是内存地址、I/O 长度和文件偏移均按
/// 逻辑块大小对齐(通常为 512 字节)。部分文件系统要求 4096 字节对齐，
/// 此处按最严格的 4096 字节校验，以避免运行时出现难以排查的 `EINVAL`。
#[cfg(target_os = "linux")]
const O_DIRECT_ALIGN: usize = 4096;

/// 校验 O_DIRECT 写入/读取的 offset 与 length 是否满足对齐要求。
#[cfg(target_os = "linux")]
fn validate_odirect_alignment(offset: u64, len: usize) -> DownloadResult<()> {
    let align = O_DIRECT_ALIGN as u64;
    if !offset.is_multiple_of(align) {
        return Err(invalid_input(format!(
            "io_uring O_DIRECT 文件偏移 {offset} 未按 {O_DIRECT_ALIGN} 字节对齐"
        )));
    }
    let len_u64 = len as u64;
    if !len_u64.is_multiple_of(align) {
        return Err(invalid_input(format!(
            "io_uring O_DIRECT I/O 长度 {len} 未按 {O_DIRECT_ALIGN} 字节对齐"
        )));
    }
    Ok(())
}

/// io_uring 实例持有者(Linux only)
///
/// P1-04: 使用 driver task 架构替代 Mutex。
/// driver task 独占 `IoUring` 实例，通过 channel 接收写入/读取/同步请求。
/// 多个并发写入请求在 driver task 内批量提交 SQE，一次 `submit_and_wait(N)`
/// 替代每个请求单独 `submit_and_wait(1)`，消除 ring 级串行化。
///
/// # 性能收益
///
/// - 批量提交：N 个并发写入只需 1 次 `submit_and_wait(N)`，而非 N 次 `submit_and_wait(1)`
/// - 消除 Mutex 竞争：driver task 独占 ring，无锁竞争
/// - 异步等待：调用方通过 oneshot channel 异步等待完成，不阻塞 tokio 工作线程
#[cfg(target_os = "linux")]
struct IoUringHandle {
    /// 驱动任务命令通道
    cmd_tx: tokio::sync::mpsc::Sender<DriverCmd>,
    /// 驱动任务 JoinHandle(Drop 时 abort + 尝试 join)
    ///
    /// H-01: IoUringStorage::drop 期间无法 .await(tokio 任务句柄的 join 需要
    /// runtime 上下文),但保留 handle 以便在 Drop 中 abort 驱动任务,避免
    /// 驱动任务在 IoUring 实例被释放后仍访问悬垂资源。
    ///
    /// 使用 std::sync::Mutex 而非 tokio::sync::Mutex:Drop 是同步的,
    /// tokio Mutex 的 lock() 返回 Future 无法在 Drop 中 .await。此处临界区
    /// 极短(仅 take 一次 JoinHandle),std::sync::Mutex 的阻塞不会跨 await 点
    /// 持有,不会导致死锁。
    driver_join: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// 注册的 fixed buffers (Arc 共享，driver task 和调用方均可访问)
    ///
    /// 调用方需要在提交 WriteReq 前将数据复制到 buffer 中，
    /// 因为 reqwest 产出的 Bytes 不满足 O_DIRECT 对齐要求。
    /// driver task 只负责构造 SQE 和提交。
    buffers: std::sync::Arc<Vec<AlignedBuffer>>,
    /// fixed buffer 分配位图(1=已占用, 0=空闲)
    ///
    /// M-01: 多字位图,支持超过 64 个 fixed buffer。
    /// 字数 = `div_ceil(buffer_count, 64)`,最后一个 word 的越界高位预置为 1
    /// (已占用),防止 alloc 分配到超出 buffer_count 的索引。
    buffer_bitmap: Box<[AtomicU64]>,
    /// buffer 数量(用于 alloc 失败时的诊断 + free 时边界检查)
    #[allow(dead_code)]
    buffer_count: usize,
}

/// io_uring 驱动任务命令
///
/// P1-04: 调用方通过此枚举向 driver task 发送操作请求，
/// driver task 独占 `IoUring` 实例批量处理。
#[cfg(target_os = "linux")]
enum DriverCmd {
    /// 写入请求
    Write(WriteReq),
    /// 读取请求
    Read(ReadReq),
    /// 同步请求(fsync)
    Sync {
        done: tokio::sync::oneshot::Sender<DownloadResult<()>>,
    },
    /// 关闭 driver task
    #[allow(dead_code)]
    Shutdown,
}

/// io_uring 写入请求
#[cfg(target_os = "linux")]
struct WriteReq {
    /// 文件偏移
    offset: u64,
    /// 待写入数据长度(字节)
    len: usize,
    /// 文件描述符
    fd: i32,
    /// fixed buffer 索引
    buf_idx: usize,
    /// 完成通知
    done: tokio::sync::oneshot::Sender<DownloadResult<usize>>,
}

/// io_uring 读取请求
#[cfg(target_os = "linux")]
struct ReadReq {
    /// 文件偏移
    offset: u64,
    /// 读取长度
    read_len: usize,
    /// 文件描述符
    fd: i32,
    /// fixed buffer 索引
    buf_idx: usize,
    /// 完成通知
    done: tokio::sync::oneshot::Sender<DownloadResult<Vec<u8>>>,
}

/// 已提交到 io_uring 但尚未收到 CQE 的请求。
/// P1-04: 使用 user_data 匹配 CQE,避免依赖 CQE 与 SQE 的顺序假设。
#[cfg(target_os = "linux")]
enum InflightReq {
    Write(WriteReq),
    Read(ReadReq),
    Sync(tokio::sync::oneshot::Sender<DownloadResult<()>>),
}

/// io_uring driver task 主体
///
/// P1-04: 独占 `IoUring` 实例，通过 channel 接收操作请求。
/// 核心优化：批量收集多个写入/读取请求，一次 `submit_and_wait(N)` 提交所有 SQE，
/// 消除 `Mutex` 串行化和逐请求 `submit_and_wait(1)` 的开销。
///
/// # 批量策略
///
/// 当收到第一个请求后，非阻塞 drain 通道中所有待处理请求，
/// 构造批量 SQE 一次性提交。在高并发场景下：
/// - N 个并发写入 → 1 次 submit (而非 N 次 submit_and_wait)
/// - 系统调用次数从 O(N) 降为 O(1)
/// - 内核可以优化批量 I/O 调度顺序
#[cfg(target_os = "linux")]
async fn driver_task(
    mut ring: io_uring::IoUring,
    mut cmd_rx: tokio::sync::mpsc::Receiver<DriverCmd>,
    buffers: std::sync::Arc<Vec<AlignedBuffer>>,
) {
    // 用于为每个 SQE 生成唯一 user_data,使 CQE 可安全匹配到原始请求。
    // 从 1 开始避免与默认值 0 混淆。
    let mut next_user_data: u64 = 1;
    // 已提交到 io_uring 但尚未收到 CQE 的请求。
    // P1-04: 不假设 CQE 顺序与 SQE 提交顺序一致,按 user_data 查找。
    let mut inflight: std::collections::HashMap<u64, InflightReq> =
        std::collections::HashMap::new();
    // 最近使用的文件描述符,供独立 Sync 请求构造 Fsync SQE 使用。
    let mut last_fd: Option<i32> = None;

    loop {
        // 1. 等待第一个命令
        let cmd = match cmd_rx.recv().await {
            Some(cmd) => cmd,
            None => break, // 通道关闭，退出
        };

        // 本批次请求
        let mut pending_writes: Vec<WriteReq> = Vec::new();
        let mut pending_reads: Vec<ReadReq> = Vec::new();
        let mut pending_syncs: Vec<tokio::sync::oneshot::Sender<DownloadResult<()>>> = Vec::new();

        // 处理第一个命令
        match cmd {
            DriverCmd::Shutdown => break,
            DriverCmd::Write(req) => pending_writes.push(req),
            DriverCmd::Read(req) => pending_reads.push(req),
            DriverCmd::Sync { done } => pending_syncs.push(done),
        }

        // 2. 非阻塞 drain：收集更多待处理请求
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                DriverCmd::Shutdown => break,
                DriverCmd::Write(req) => pending_writes.push(req),
                DriverCmd::Read(req) => pending_reads.push(req),
                DriverCmd::Sync { done } => pending_syncs.push(done),
            }
        }

        // 3. 批量提交写入 SQE
        // P1-04: 调用方已将数据复制到 fixed buffer，driver task 只需构造 SQE
        if !pending_writes.is_empty() {
            let mut sq = ring.submission();
            for req in pending_writes {
                last_fd = Some(req.fd);
                let buf = &buffers[req.buf_idx];
                let user_data = next_user_data;

                let write_op = io_uring::opcode::WriteFixed::new(
                    io_uring::types::Fd(req.fd),
                    buf.ptr() as *const u8,
                    req.len as u32,
                    req.buf_idx as u16,
                )
                .offset(req.offset)
                .build()
                .user_data(user_data);

                // SAFETY: write_op 由 WriteFixed::build() 构造，是有效的 SQE
                // 数据已由调用方复制到 fixed buffer，生命周期安全
                unsafe {
                    if sq.push(&write_op).is_ok() {
                        next_user_data = next_user_data.wrapping_add(1);
                        inflight.insert(user_data, InflightReq::Write(req));
                    } else {
                        let _ = req.done.send(Err(DownloadError::Io(std::io::Error::other(
                            "io_uring 提交队列已满",
                        ))));
                    }
                }
            }
            sq.sync();
            drop(sq);
        }

        // 4. 批量提交读取 SQE
        if !pending_reads.is_empty() {
            let mut sq = ring.submission();
            for req in pending_reads {
                last_fd = Some(req.fd);
                let buf = &buffers[req.buf_idx];
                let actual_len = req.read_len.min(buf.len());
                let user_data = next_user_data;

                let read_op = io_uring::opcode::ReadFixed::new(
                    io_uring::types::Fd(req.fd),
                    buf.ptr(),
                    actual_len as u32,
                    req.buf_idx as u16,
                )
                .offset(req.offset)
                .build()
                .user_data(user_data);

                // SAFETY: read_op 由 ReadFixed::build() 构造，是有效的 SQE
                unsafe {
                    if sq.push(&read_op).is_ok() {
                        next_user_data = next_user_data.wrapping_add(1);
                        inflight.insert(user_data, InflightReq::Read(req));
                    } else {
                        let _ = req.done.send(Err(DownloadError::Io(std::io::Error::other(
                            "io_uring 提交队列已满",
                        ))));
                    }
                }
            }
            sq.sync();
            drop(sq);
        }

        // 5. 批量提交同步 SQE
        if !pending_syncs.is_empty() {
            if let Some(fd) = last_fd {
                let mut sq = ring.submission();
                for done in pending_syncs {
                    let user_data = next_user_data;
                    let fsync_op = io_uring::opcode::Fsync::new(io_uring::types::Fd(fd))
                        .build()
                        .user_data(user_data);
                    // SAFETY: fsync_op 由 Fsync::build() 构造
                    unsafe {
                        if sq.push(&fsync_op).is_ok() {
                            next_user_data = next_user_data.wrapping_add(1);
                            inflight.insert(user_data, InflightReq::Sync(done));
                        } else {
                            let _ = done.send(Err(DownloadError::Io(std::io::Error::other(
                                "io_uring 提交队列已满",
                            ))));
                        }
                    }
                }
                sq.sync();
                drop(sq);
            } else {
                // 无 fd 可用时无法执行 fsync,直接返回错误
                for done in pending_syncs {
                    let _ = done.send(Err(DownloadError::Io(std::io::Error::other(
                        "io_uring sync 缺少文件描述符",
                    ))));
                }
            }
        }

        // 6. 计算总 SQE 数量并一次性提交
        let total_sqes = inflight.len();
        if total_sqes == 0 {
            continue;
        }

        // submit_and_wait: 提交所有 SQE 并等待全部完成
        if ring.submitter().submit_and_wait(total_sqes).is_err() {
            // 提交失败：通知所有 inflight 请求
            for (_, req) in inflight.drain() {
                match req {
                    InflightReq::Write(r) => {
                        let err = Err(DownloadError::Io(std::io::Error::other(
                            "io_uring submit_and_wait 失败",
                        )));
                        let _ = r.done.send(err);
                    }
                    InflightReq::Read(r) => {
                        let err = Err(DownloadError::Io(std::io::Error::other(
                            "io_uring submit_and_wait 失败",
                        )));
                        let _ = r.done.send(err);
                    }
                    InflightReq::Sync(done) => {
                        let err = Err(DownloadError::Io(std::io::Error::other(
                            "io_uring submit_and_wait 失败",
                        )));
                        let _ = done.send(err);
                    }
                }
            }
            continue;
        }

        // 7. 收集 CQE 并按 user_data 分发结果
        let mut cq = ring.completion();
        for cqe in cq.by_ref() {
            let user_data = cqe.user_data();
            let r = cqe.result();
            let Some(req) = inflight.remove(&user_data) else {
                tracing::warn!(user_data, "io_uring CQE user_data 无匹配请求");
                continue;
            };

            match req {
                InflightReq::Write(write_req) => {
                    let result = if r < 0 {
                        Err(DownloadError::Io(std::io::Error::from_raw_os_error(-r)))
                    } else {
                        Ok(r as usize)
                    };
                    let _ = write_req.done.send(result);
                }
                InflightReq::Read(read_req) => {
                    let result = if r < 0 {
                        Err(DownloadError::Io(std::io::Error::from_raw_os_error(-r)))
                    } else {
                        let bytes_read = r as usize;
                        // 从 fixed buffer 复制到 Vec 返回
                        let buf = &buffers[read_req.buf_idx];
                        let src = unsafe { std::slice::from_raw_parts(buf.as_ptr(), bytes_read) };
                        Ok(src.to_vec())
                    };
                    let _ = read_req.done.send(result);
                }
                InflightReq::Sync(done) => {
                    let result = if r < 0 {
                        Err(DownloadError::Io(std::io::Error::from_raw_os_error(-r)))
                    } else {
                        Ok(())
                    };
                    let _ = done.send(result);
                }
            }
        }

        // 若 CQE 缺失,通知剩余 inflight 请求
        if !inflight.is_empty() {
            tracing::warn!(remaining = inflight.len(), "io_uring CQE 缺失");
            for (_, req) in inflight.drain() {
                match req {
                    InflightReq::Write(r) => {
                        let err = Err(DownloadError::Io(std::io::Error::other("CQE 缺失")));
                        let _ = r.done.send(err);
                    }
                    InflightReq::Read(r) => {
                        let err = Err(DownloadError::Io(std::io::Error::other("CQE 缺失")));
                        let _ = r.done.send(err);
                    }
                    InflightReq::Sync(done) => {
                        let err = Err(DownloadError::Io(std::io::Error::other("CQE 缺失")));
                        let _ = done.send(err);
                    }
                }
            }
        }
    }

    // driver task 退出，drain 剩余命令并返回错误
    tracing::info!("io_uring driver task 退出");
}

#[cfg(target_os = "linux")]
impl IoUringHandle {
    /// 原子分配一个空闲 fixed buffer 索引。
    ///
    /// 位图不变量: 每一位对应一个 fixed buffer, `1` = 已占用, `0` = 空闲。
    /// 超出 `buffers.len()` 的高位在初始化时被预置为 `1`(见 `init` 中的
    /// `build_buffer_bitmap`),因此本函数只会选中 `[0, buffers.len()-1]` 范围内的位。
    ///
    /// M-01: 支持多字位图(超过 64 个 buffer)。遍历每个 word,对其执行
    /// `(!current).trailing_zeros()` 找空闲位,CAS 占用。全局索引 =
    /// `word_idx * 64 + bit`,边界由 `buffer_count` 兜底校验。
    ///
    /// 返回的索引保证落在 `buffers` 内,当所有 buffer 都被占用时返回 None。
    fn alloc_buffer_index(&self) -> Option<usize> {
        bitmap_alloc_first_free(&self.buffer_bitmap, self.buffer_count)
    }

    /// 释放 fixed buffer 索引,使其可被后续操作重新分配。
    ///
    /// M-01: 多字位图下计算 `word_idx = idx / 64`、`bit = idx % 64`,
    /// 对相应 word 执行 CAS 清位。idx 越界(>= buffer_count)时静默忽略,
    /// 防止误清非本 handle 管辖的位。
    fn free_buffer_index(&self, idx: usize) {
        if idx >= self.buffer_count {
            return;
        }
        let word_idx = idx / 64;
        let bit = idx % 64;
        // word_idx < buffer_bitmap.len() 由 init 保证(div_ceil(buffer_count,64)),
        // 且 idx < buffer_count 时 word_idx 一定在位图范围内。
        self.buffer_bitmap[word_idx].fetch_and(!(1u64 << bit), Ordering::Relaxed);
    }
}

/// 构造 fixed buffer 分配位图(Box<[AtomicU64]>)。
///
/// M-01: 多字位图。字数 = `div_ceil(buffer_count, 64)`。
/// 位图语义: `0` = 空闲, `1` = 已占用。最后一个 word 中超出 `buffer_count`
/// 的高位预置为 `1`(已占用),防止 `alloc` 分配到越界索引——与 `iocp.rs`
/// 的 `free_bitmap` 初始化模式一致(见 `CompletionSlots::new`)。
#[cfg(target_os = "linux")]
fn build_buffer_bitmap(buffer_count: usize) -> Box<[AtomicU64]> {
    let words = buffer_count.div_ceil(64);
    // Safety: words == 0 当且仅当 buffer_count == 0,但 init() 在此之前已通过
    // validate_fixed_buffer_config 拒绝 buffer_count == 0,故 words >= 1。
    (0..words)
        .map(|word_idx| {
            // 本 word 覆盖的位范围: [word_idx*64, word_idx*64 + 64)
            // excess = 本 word 末位索引 - buffer_count,>0 表示有越界高位。
            // 使用 i64 运算避免 usize 减法溢出(与 iocp.rs::CompletionSlots::new
            // 的 free_bitmap 初始化模式一致)。
            let excess = (word_idx as i64 + 1) * 64 - buffer_count as i64;
            if excess >= 64 {
                // 本 word 全部落在 buffer_count 范围内:全空闲
                AtomicU64::new(0)
            } else if excess > 0 {
                // 最后一个 word: 越界高位预置为已占用
                AtomicU64::new((!0u64) << (64 - excess as usize))
            } else {
                // excess <= 0:本 word 全部有效,全空闲
                AtomicU64::new(0)
            }
        })
        .collect()
}

/// 在多字 AtomicU64 位图上无锁查找并占用第一个空闲位。
///
/// M-01: 遍历每个 word,对 `current` 取反后 `trailing_zeros()` 找第一个 0 位
/// (即空闲位),CAS 设置该位以原子声明占用。全局索引 = `word_idx * 64 + bit`。
///
/// 位图不变量: `1` = 已占用, `0` = 空闲。超出 `buffer_count` 的高位由
/// `build_buffer_bitmap` 预置为 `1`,此处再做 `idx >= buffer_count` 兜底校验。
///
/// 返回的索引保证 `< buffer_count`(超出范围则返回 None)。
#[cfg(target_os = "linux")]
fn bitmap_alloc_first_free(bitmap: &[AtomicU64], buffer_count: usize) -> Option<usize> {
    for (word_idx, word) in bitmap.iter().enumerate() {
        let mut current = word.load(Ordering::Relaxed);
        loop {
            if current == u64::MAX {
                break; // 本 word 已满,继续下一个 word
            }
            // 取反后 trailing_zeros 给出第一个空闲位(原值的第一个 0)。
            let bit = (!current).trailing_zeros() as usize;
            let idx = word_idx * 64 + bit;
            // 防御性边界: 超出实际 buffer 数量的位不应被选中
            // (正常情况下 build_buffer_bitmap 已预占高位,此处为双重保险)
            if idx >= buffer_count {
                return None;
            }
            let next = current | (1u64 << bit);
            match word.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return Some(idx),
                Err(actual) => current = actual,
            }
        }
    }
    None
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

        // P1-04: 将 buffers 包装为 Arc，IoUringHandle 和 driver task 共享
        let buffers_arc = std::sync::Arc::new(buffers);
        let buffer_count = buffers_arc.len();

        // P1-04: 启动 driver task，替代 Mutex 串行化
        // driver task 独占 IoUring 实例，通过 channel 接收操作请求，
        // 批量提交 SQE，一次 submit_and_wait(N) 替代逐请求 submit_and_wait(1)
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<DriverCmd>(256);
        let driver_buffers = std::sync::Arc::clone(&buffers_arc);

        let driver_join = tokio::spawn(async move {
            driver_task(ring, cmd_rx, driver_buffers).await;
        });

        // M-01: 多字位图,支持超过 64 个 fixed buffer。
        let buffer_bitmap = build_buffer_bitmap(buffer_count);

        self.ring = Some(std::sync::Arc::new(IoUringHandle {
            cmd_tx,
            driver_join: std::sync::Mutex::new(Some(driver_join)),
            buffers: buffers_arc,
            buffer_bitmap,
            buffer_count,
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

    /// 提交读取操作到 io_uring (Linux)
    ///
    /// P1-04: 通过 driver task channel 发送读取请求。
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

        let read_len = buf.len();
        validate_odirect_alignment(offset, read_len)?;

        // 分配 fixed buffer 索引
        let buf_idx = ring_handle.alloc_buffer_index().ok_or_else(|| {
            DownloadError::Io(std::io::Error::other(
                "io_uring fixed buffer 已耗尽,并发读取操作过多",
            ))
        })?;

        let (done_tx, done_rx) = tokio::sync::oneshot::channel();

        ring_handle
            .cmd_tx
            .send(DriverCmd::Read(ReadReq {
                offset,
                read_len,
                fd,
                buf_idx,
                done: done_tx,
            }))
            .await
            .map_err(|_| DownloadError::Io(std::io::Error::other("io_uring driver task 已关闭")))?;

        let read_result: Vec<u8> = done_rx
            .await
            .map_err(|_| DownloadError::Io(std::io::Error::other("io_uring 读取完成通知丢失")))??;

        // 释放 buffer 索引
        ring_handle.free_buffer_index(buf_idx);

        // 从返回的 Vec 复制到用户缓冲区
        let bytes_read = read_result.len();
        buf[..bytes_read].copy_from_slice(&read_result);
        Ok(bytes_read)
    }

    /// 同步文件数据到磁盘 (Linux)
    ///
    /// P1-04: 通过 driver task channel 发送同步请求。
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

        let (done_tx, done_rx) = tokio::sync::oneshot::channel();

        ring_handle
            .cmd_tx
            .send(DriverCmd::Sync { done: done_tx })
            .await
            .map_err(|_| DownloadError::Io(std::io::Error::other("io_uring driver task 已关闭")))?;

        done_rx
            .await
            .map_err(|_| DownloadError::Io(std::io::Error::other("io_uring 同步完成通知丢失")))?
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

    /// 提交写入操作到 io_uring (Linux)
    ///
    /// P1-04: 通过 driver task channel 发送写入请求。
    /// 调用方先分配 buffer 索引并将数据复制到 fixed buffer，
    /// 然后通过 channel 发送 WriteReq，driver task 批量收集并提交 SQE。
    /// 调用方通过 oneshot channel 异步等待完成结果。
    #[cfg(target_os = "linux")]
    async fn submit_write(&self, offset: u64, data: &[u8]) -> DownloadResult<usize> {
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

        let len = data.len();
        let buffer_len = ring_handle
            .buffers
            .first()
            .map(AlignedBuffer::len)
            .ok_or_else(|| invalid_input("io_uring has no registered fixed buffers for write"))?;
        validate_fixed_buffer_write_len(len, buffer_len)?;
        validate_odirect_alignment(offset, len)?;

        // 分配 fixed buffer 索引
        let buf_idx = ring_handle.alloc_buffer_index().ok_or_else(|| {
            DownloadError::Io(std::io::Error::other(
                "io_uring fixed buffer 已耗尽,并发写入操作过多",
            ))
        })?;

        // 将数据复制到 fixed buffer（必须在发送请求前完成，
        // 因为 driver task 会直接引用 buffer 地址构造 SQE）
        let buf = &ring_handle.buffers[buf_idx];
        // Safety: alloc_buffer_index 保证同一时刻只有一个操作使用该 buffer 索引
        let dst = unsafe { std::slice::from_raw_parts_mut(buf.ptr(), len) };
        dst.copy_from_slice(data);

        let (done_tx, done_rx) = tokio::sync::oneshot::channel();

        ring_handle
            .cmd_tx
            .send(DriverCmd::Write(WriteReq {
                offset,
                len,
                fd,
                buf_idx,
                done: done_tx,
            }))
            .await
            .map_err(|_| DownloadError::Io(std::io::Error::other("io_uring driver task 已关闭")))?;

        let result = done_rx
            .await
            .map_err(|_| DownloadError::Io(std::io::Error::other("io_uring 写入完成通知丢失")))?;

        // 无论成功失败都释放 buffer 索引
        ring_handle.free_buffer_index(buf_idx);

        result
    }
}

/// H-01: IoUringStorage 的 Drop 实现。
///
/// io_uring 驱动以独立 tokio task 运行,独占 `IoUring` 实例并通过 channel
/// 接收操作请求。若不在 `IoUringStorage` 析构时通知驱动退出,则:
///   1. 驱动 task 持有 `IoUring`(含已注册 fixed buffers 的内核映射)和
///      `buffers: Arc<Vec<AlignedBuffer>>`,在 Storage drop 后仍存活,
///      驱动可能在 IoUring / buffers 被释放后继续访问悬垂资源;
///   2. IoUringHandle 的 Arc 引用计数不会归零,buffer_bitmap / channel
///      等资源泄漏,直到驱动 task 自行结束(可能永不结束)。
///
/// Drop 策略(参照 `iocp.rs` 的 cancel + drain + join 模式,适配 tokio task):
///   1. 尝试发送 `DriverCmd::Shutdown` 让驱动优雅退出(非阻塞——
///      Drop 是同步的,不能 `.await`,故用 `try_send`);
///   2. 若发送失败(channel 已满或驱动已退出)或驱动未及时结束,调用
///      `JoinHandle::abort()` 强制取消驱动 task,避免资源泄漏;
///   3. 轮询 `is_finished()` 短暂等待(最长约 100ms),给驱动一个收尾机会,
///      但不阻塞调用方过久——这与 `iocp.rs::drain_pending_completions`
///      的有界等待意图一致。
///
/// 注意:Drop 中不能 `.await`(无 async 上下文),故 join 采用同步轮询而非
/// `JoinHandle::await`。即使驱动未在窗口内退出,`abort()` 也已请求取消,
/// 驱动持有的 Arc 引用会在 task 真正结束后随 Arc 计数归零而释放。
#[cfg(target_os = "linux")]
impl Drop for IoUringStorage {
    fn drop(&mut self) {
        let Some(handle) = self.ring.take() else {
            return; // 未初始化(init 失败或从未调用),无驱动需清理
        };

        // 1. 优雅退出:非阻塞发送 Shutdown 命令。
        //    try_send 不阻塞;若 channel 满,下方 abort 兜底。
        if handle.cmd_tx.try_send(DriverCmd::Shutdown).is_err() {
            tracing::debug!("io_uring drop: Shutdown 命令发送失败,将直接 abort 驱动 task");
        }

        // 2. 取出 JoinHandle 并 abort,确保驱动不会在资源释放后继续运行。
        //    driver_join 用 std::sync::Mutex<Option<_>> 包裹:Drop 中 take 出
        //    JoinHandle。锁可能因 panic 中毒,此处 unwrap_or_else into_inner
        //    安全恢复(仅取 JoinHandle,无内存安全问题)。
        let join_handle = handle
            .driver_join
            .lock()
            .map_or_else(|e| e.into_inner().take(), |mut guard| guard.take());
        if let Some(jh) = join_handle {
            // 若驱动已自行退出则无需 abort;否则 abort 请求取消。
            if !jh.is_finished() {
                jh.abort();
                // 3. 有界等待驱动退出(同步轮询,最多 ~100ms)。
                //    给驱动一个收尾窗口,但避免在 Drop 中长时间阻塞。
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
                while !jh.is_finished() && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                if !jh.is_finished() {
                    tracing::warn!("io_uring 驱动 task 在 drop 超时窗口内未退出,已请求 abort");
                }
            }
        }

        tracing::debug!("io_uring storage 已 drop,驱动 task 清理完成");
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
                        // 自动处理 O_DIRECT 对齐:非对齐写入通过填充零字节对齐
                        let align = O_DIRECT_ALIGN as u64;
                        let align_mask = align - 1;
                        let data_len = _data.len() as u64;
                        let is_aligned =
                            _offset.is_multiple_of(align) && data_len.is_multiple_of(align);

                        if is_aligned {
                            // 快速路径:已对齐,直接写入
                            validate_fixed_buffer_write_len(_data.len(), self.config.buffer_size)?;
                            return self.submit_write(_offset, &_data).await;
                        }

                        // 慢速路径:非对齐写入,自动填充对齐
                        let aligned_offset = _offset & !align_mask;
                        let front_pad = (_offset - aligned_offset) as usize;
                        let total_len = front_pad + _data.len();
                        let padded_len = ((total_len as u64 + align_mask) & !align_mask) as usize;

                        // 如果填充后超过 fixed buffer 大小,回退到 TokioFile
                        if padded_len > self.config.buffer_size {
                            return Err(DownloadError::Io(std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                format!(
                                    "io_uring 非对齐写入填充后 {padded_len} 字节超过 fixed buffer 大小 {}",
                                    self.config.buffer_size
                                ),
                            )));
                        }

                        // 构造对齐缓冲区:前填充零 + 用户数据 + 后填充零
                        let mut padded = vec![0u8; padded_len];
                        padded[front_pad..front_pad + _data.len()].copy_from_slice(&_data);

                        validate_fixed_buffer_write_len(padded_len, self.config.buffer_size)?;
                        let written = self.submit_write(aligned_offset, &padded).await?;
                        // submit_write 返回 padded 的写入量(含前/后填充零),
                        // 调用方期望的是用户数据字节数。O_DIRECT 对齐写入通常一次完成
                        // (padded 全部写入),此时用户数据完整覆盖,返回 _data.len()。
                        // 若短写未覆盖全部用户数据,按实际覆盖量返回(扣除 front_pad 偏移)。
                        let user_written = written.saturating_sub(front_pad).min(_data.len());
                        Ok(user_written)
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

    fn write_at_mut<'a>(
        &'a self,
        _offset: u64,
        _data: &'a mut BytesMut,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
        Box::pin(async move {
            match self.state {
                IoUringState::Ready => {
                    #[cfg(target_os = "linux")]
                    {
                        let align = O_DIRECT_ALIGN as u64;
                        let align_mask = align - 1;
                        let data_len = _data.len() as u64;
                        let is_aligned =
                            _offset.is_multiple_of(align) && data_len.is_multiple_of(align);

                        if is_aligned {
                            validate_fixed_buffer_write_len(_data.len(), self.config.buffer_size)?;
                            return self.submit_write(_offset, _data).await;
                        }

                        let aligned_offset = _offset & !align_mask;
                        let front_pad = (_offset - aligned_offset) as usize;
                        let total_len = front_pad + _data.len();
                        let padded_len = ((total_len as u64 + align_mask) & !align_mask) as usize;

                        if padded_len > self.config.buffer_size {
                            return Err(DownloadError::Io(std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                format!(
                                    "io_uring 非对齐写入填充后 {padded_len} 字节超过 fixed buffer 大小 {}",
                                    self.config.buffer_size
                                ),
                            )));
                        }

                        let mut padded = vec![0u8; padded_len];
                        padded[front_pad..front_pad + _data.len()].copy_from_slice(_data);

                        validate_fixed_buffer_write_len(padded_len, self.config.buffer_size)?;
                        self.submit_write(aligned_offset, &padded).await
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
            // M-02: fallocate 的 len 参数为 i64(off_t)。若 size > i64::MAX,
            // `size as i64` 会静默截断为负数,导致 fallocate 行为未定义或 EINVAL。
            // 在入口处显式校验,拒绝溢出的 size。
            #[cfg(target_os = "linux")]
            i64::try_from(size).map_err(|_| {
                invalid_input(format!(
                    "io_uring allocate size {size} 超过 i64 最大值 {},fallocate 无法处理",
                    i64::MAX
                ))
            })?;
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
            .write_at(0, Bytes::from(vec![0u8; 8192]))
            .await
            .expect_err("超过 fixed buffer 大小时 write_at 必须先返回错误");

        // 对齐 payload (8192) 走快速路径, validate_fixed_buffer_write_len 直接拒绝
        // 给出 "exceeds fixed buffer size" 错误。非对齐 payload 会走 padding 路径
        // 产生不同错误消息, 故此处使用对齐尺寸验证快速路径前置校验语义。
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

    /// 回归测试: buffer_count=16 时,首次分配必须返回 idx=0 而非 16。
    ///
    /// 此前 bug: `alloc_buffer_index` 直接用 `current.trailing_zeros()` 取
    /// 第一个置位(已占用)位,与"找第一个空闲位"语义相反。
    /// 初始化 used_mask = (!0u64) << 16 = 0xFFFFFFFFFFFF0000 时,
    /// trailing_zeros 返回 16, 导致 buffers[16] 越界 panic。
    #[cfg(target_os = "linux")]
    #[test]
    fn test_bitmap_alloc_first_free_returns_zero_with_initial_used_mask() {
        let buffer_count = 16;
        // M-01: build_buffer_bitmap 构造多字位图,buffer_count=16 时为单 word,
        // 高位(16-63)预置为 1(已占用)。
        let bitmap: Box<[AtomicU64]> = build_buffer_bitmap(buffer_count);

        let idx = bitmap_alloc_first_free(&bitmap, buffer_count);
        assert_eq!(idx, Some(0), "首次分配必须返回索引 0");

        // 第二次分配应返回 1
        let idx2 = bitmap_alloc_first_free(&bitmap, buffer_count);
        assert_eq!(idx2, Some(1), "第二次分配应返回索引 1");
    }

    /// 回归测试: 所有 buffer 占用时返回 None,不越界。
    #[cfg(target_os = "linux")]
    #[test]
    fn test_bitmap_alloc_first_free_returns_none_when_all_used() {
        let buffer_count = 4;
        let bitmap: Box<[AtomicU64]> = build_buffer_bitmap(buffer_count);
        // 占满全部 4 个有效槽
        for _ in 0..buffer_count {
            assert!(
                bitmap_alloc_first_free(&bitmap, buffer_count).is_some(),
                "未占满时应可分配"
            );
        }
        assert_eq!(
            bitmap_alloc_first_free(&bitmap, buffer_count),
            None,
            "占满后应返回 None"
        );
    }

    /// 回归测试: 防御性边界检查 - 超出 buffer_count 的位不应被分配。
    #[cfg(target_os = "linux")]
    #[test]
    fn test_bitmap_alloc_first_free_respects_buffer_count_boundary() {
        let buffer_count = 8;
        let bitmap: Box<[AtomicU64]> = build_buffer_bitmap(buffer_count);
        // 占满 0-7 后, 即使位图高位为 0(build_buffer_bitmap 已预占),
        // 也不应返回越界索引
        for expected in 0..buffer_count {
            assert_eq!(
                bitmap_alloc_first_free(&bitmap, buffer_count),
                Some(expected)
            );
        }
        assert_eq!(
            bitmap_alloc_first_free(&bitmap, buffer_count),
            None,
            "超出 buffer_count 的位不应被分配"
        );
    }

    /// 回归测试: 释放后可重新分配。
    #[cfg(target_os = "linux")]
    #[test]
    fn test_bitmap_alloc_free_reuse_cycle() {
        let buffer_count = 4;
        let bitmap: Box<[AtomicU64]> = build_buffer_bitmap(buffer_count);

        // 占满 4 个槽
        for expected in 0..buffer_count {
            assert_eq!(
                bitmap_alloc_first_free(&bitmap, buffer_count),
                Some(expected)
            );
        }
        assert_eq!(bitmap_alloc_first_free(&bitmap, buffer_count), None);

        // 释放索引 2 (清除 word 0 的 bit 2)
        bitmap[0].fetch_and(!(1u64 << 2), Ordering::Relaxed);
        assert_eq!(bitmap_alloc_first_free(&bitmap, buffer_count), Some(2));
    }

    /// M-01: 多字位图测试 - buffer_count=128 时跨 2 个 word。
    ///
    /// 验证:
    /// 1. build_buffer_bitmap 生成 2 个 word(div_ceil(128,64)=2);
    /// 2. 第一个 word 全 0,第二个 word 全 0(128 恰为 64 倍数,无越界高位);
    /// 3. 可分配 0..128 全部索引,不越界;
    /// 4. 占满后返回 None。
    #[cfg(target_os = "linux")]
    #[test]
    fn test_bitmap_multi_word_alloc_across_word_boundary() {
        let buffer_count = 128;
        let bitmap: Box<[AtomicU64]> = build_buffer_bitmap(buffer_count);
        // div_ceil(128, 64) = 2 words
        assert_eq!(bitmap.len(), 2, "128 个 buffer 应使用 2 个 word");
        // 128 是 64 的整数倍,无越界高位,两个 word 均应初始化为 0
        assert_eq!(bitmap[0].load(Ordering::Relaxed), 0);
        assert_eq!(bitmap[1].load(Ordering::Relaxed), 0);

        // 分配 word 0 的最后一位(idx=63)与 word 1 的第一位(idx=64),
        // 验证跨 word 边界正确推进
        for expected in 0..buffer_count {
            assert_eq!(
                bitmap_alloc_first_free(&bitmap, buffer_count),
                Some(expected),
                "应顺序分配索引 {expected}"
            );
        }
        // 占满后返回 None
        assert_eq!(
            bitmap_alloc_first_free(&bitmap, buffer_count),
            None,
            "128 个槽占满后应返回 None"
        );
    }

    /// M-01: 多字位图测试 - buffer_count=70(非 64 倍数)时,
    /// 第二个 word 的越界高位(位 70-127)被预置为 1(已占用),
    /// 不会分配到 idx >= 70 的索引。
    #[cfg(target_os = "linux")]
    #[test]
    fn test_bitmap_multi_word_excess_bits_preoccupied() {
        let buffer_count = 70;
        let bitmap: Box<[AtomicU64]> = build_buffer_bitmap(buffer_count);
        assert_eq!(bitmap.len(), 2, "70 个 buffer 应使用 2 个 word");
        // 第二个 word: 位 0-5 对应 idx 64-69(有效),位 6-63 越界预置为 1
        // excess = 2*64 - 70 = 58,故 mask = (!0u64) << (64 - 58) = (!0u64) << 6
        assert_eq!(
            bitmap[1].load(Ordering::Relaxed),
            (!0u64) << 6,
            "第二个 word 的越界高位应被预置为已占用"
        );

        // 顺序分配 idx 0-69(word 0 全部 64 位 + word 1 的位 0-5),
        // 共 70 个有效槽
        for expected in 0..buffer_count {
            assert_eq!(
                bitmap_alloc_first_free(&bitmap, buffer_count),
                Some(expected),
                "应顺序分配 idx {expected}"
            );
        }
        // 70 个有效槽全部占满后,第二个 word 的越界高位(6-63)已被预占,
        // 应返回 None,绝不返回 idx >= 70
        assert_eq!(
            bitmap_alloc_first_free(&bitmap, buffer_count),
            None,
            "占满 70 个有效槽后应返回 None,不越界"
        );
    }

    /// M-01: 多字位图测试 - 释放跨 word 的索引后可重新分配。
    #[cfg(target_os = "linux")]
    #[test]
    fn test_bitmap_multi_word_free_and_realloc() {
        let buffer_count = 96;
        let bitmap: Box<[AtomicU64]> = build_buffer_bitmap(buffer_count);

        // 顺序分配 idx 0-63(word 0 全部 64 位),再分配 idx 64(word 1 bit 0)
        for expected in 0..=64 {
            assert_eq!(
                bitmap_alloc_first_free(&bitmap, buffer_count),
                Some(expected),
                "应顺序分配 idx {expected}"
            );
        }
        // 释放 idx=64: word_idx=1, bit=0
        bitmap[1].fetch_and(!(1u64 << 0), Ordering::Relaxed);
        // 释放后 idx=64 重新成为第一个空闲位(word 0 已满,word 1 bit 0 空闲)
        assert_eq!(
            bitmap_alloc_first_free(&bitmap, buffer_count),
            Some(64),
            "释放 idx=64 后应重新分配到 64"
        );
    }
}
