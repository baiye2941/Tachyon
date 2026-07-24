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
#[cfg(any(test, target_os = "linux"))]
use std::sync::atomic::{AtomicU64, Ordering};

use tachyon_core::{DownloadError, DownloadResult};

use crate::storage::AsyncStorage;

/// io_uring 引擎配置
///
/// 控制提交队列深度、完成队列深度、fixed buffer 参数和 SQPOLL 行为。
/// 默认配置适合高吞吐量下载场景(256KB buffer x 16 个 = 4MB 总量)。
/// buffer_size 与引擎 WRITE_BATCH_BYTES(256KB)对齐,确保对齐快速路径
/// 能直接处理引擎产出的写入批,无需降级到非对齐 RMW 慢速路径。
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
            buffer_size: 256 * 1024, // 256KB per buffer,与 WRITE_BATCH_BYTES 对齐
            buffer_count: 16,        // 16 个 fixed buffer = 4MB 总量
            sqpoll: false,           // 默认关闭(需要 CAP_SYS_ADMIN)
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
    /// 非对齐写入读-改-写(RMW)临界区串行化锁
    ///
    /// 为什么需要锁:RMW 路径先读回对齐块、覆盖用户数据区间、再整块写回,
    /// 这是非原子序列。两个并发 RMW 落在同一对齐块时会产生 lost-update:
    /// A 读块 → B 读块 → A 写块 → B 写块,B 的写回覆盖 A 的修改。
    /// 用锁串行化"读-改-写"临界区即可消除数据竞争。
    ///
    /// 为什么用 tokio::sync::Mutex:RMW 临界区内的 submit_read/submit_write
    /// 都是 async(经 channel 向 driver task 发命令并 await 结果),guard 必须
    /// 跨 await 点持有。std::sync::Mutex 的 guard 不可跨 await(持锁 await 会
    /// 阻塞 tokio 工作线程甚至死锁),tokio::sync::Mutex 的 guard 可安全跨 await。
    ///
    /// 为什么只锁慢速路径:对齐快速路径单次 submit_write 是原子的(单条 SQE,
    /// offset 由 SQE.offset 指定,内核保证不交错),无需锁。锁粒度只覆盖非对齐
    /// RMW 路径,对齐写入零串行化,保持高并发吞吐。
    #[cfg(target_os = "linux")]
    write_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
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

// SAFETY: AlignedBuffer 始终在 driver task 架构下使用，
// 保证同一 buffer 的并发访问被 driver task 串行化。
#[cfg(target_os = "linux")]
unsafe impl Send for AlignedBuffer {}
// SAFETY: 同 Send — driver 串行化对 pending buffer 的访问，无跨线程别名写。
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
    /// fixed buffer 索引分配池(1=已占用, 0=空闲)。
    ///
    /// 由调用方(driver 提交前)与 driver task(CQE 完成时)共享:索引所有权在
    /// 命令发送给 driver 时转移,driver 在 CQE 完成时回收,避免与内核 in-flight
    /// op 竞争同一 buffer(参见 `IoUringBufferGuard`)。
    pool: std::sync::Arc<BufferIndexPool>,
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
    pool: std::sync::Arc<BufferIndexPool>,
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
            DriverCmd::Shutdown => {
                // F-04: 正常退出前 reset pool,回收异常路径泄漏的索引。
                pool.reset();
                break;
            }
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

                // SAFETY:
                // - write_op 由 WriteFixed::build() 构造,是符合 io_uring ABI 的有效 SQE。
                // - req.fd 来自合法打开的 Arc<File>(submit_write 中 as_raw_fd),
                //   IoUringStorage 持有 file_fd 的 Arc 副本,fd 在 SQE 处理期间有效。
                // - req.buf_idx 经 alloc_buffer_index 分配且尚未释放,driver task 此刻
                //   是该 fixed buffer 的唯一消费者;buf.ptr() 指向 buffers[buf_idx] 的
                //   对齐地址,数据已由调用方在 submit_write 中复制完成,push 后 SQE 才被
                //   内核消费,不存在悬垂引用。
                // - sq 是本地 SubmissionQueue,driver task 单线程独占,无并发 push。
                unsafe {
                    if sq.push(&write_op).is_ok() {
                        next_user_data = next_user_data.wrapping_add(1);
                        inflight.insert(user_data, InflightReq::Write(req));
                    } else {
                        // SQE 未能入队,内核不会处理该 op,fixed buffer 不在途。
                        // 调用方守卫 submitted=true 不会回收,此处由 driver 回收索引。
                        pool.free(req.buf_idx);
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

                // SAFETY:
                // - read_op 由 ReadFixed::build() 构造,是符合 io_uring ABI 的有效 SQE。
                // - req.fd 来自合法打开的 Arc<File>(submit_read 中 as_raw_fd),
                //   IoUringStorage 持有 file_fd 的 Arc 副本,fd 在 SQE 处理期间有效。
                // - req.buf_idx 经 alloc_buffer_index 分配且尚未释放,driver task 此刻
                //   是该 fixed buffer 的唯一消费者;buf.ptr() 指向 buffers[buf_idx] 的
                //   对齐地址,actual_len = min(read_len, buf.len()),写入长度不越界,
                //   内核 ReadFixed 完成后数据落在该 buffer 内,CQE 处理时再读出。
                // - sq 是本地 SubmissionQueue,driver task 单线程独占,无并发 push。
                unsafe {
                    if sq.push(&read_op).is_ok() {
                        next_user_data = next_user_data.wrapping_add(1);
                        inflight.insert(user_data, InflightReq::Read(req));
                    } else {
                        // SQE 未能入队,内核不会处理该 op,fixed buffer 不在途。
                        // 调用方守卫 submitted=true 不会回收,此处由 driver 回收索引。
                        pool.free(req.buf_idx);
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
                    // SAFETY:
                    // - fsync_op 由 Fsync::build() 构造,是符合 io_uring ABI 的有效 SQE。
                    // - fd 来自最近一次 Write/Read 请求的 req.fd(合法 Arc<File> 的
                    //   raw fd),IoUringStorage 持有 file_fd 的 Arc 副本,fd 在 SQE 处理
                    //   期间有效。fsync 操作不引用任何用户 buffer,无缓冲区生命周期问题。
                    // - sq 是本地 SubmissionQueue,driver task 单线程独占,无并发 push。
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

        // submit_and_wait: 提交所有 SQE 并等待全部完成。
        // 审计 M-01:该调用是同步阻塞 syscall;在 async driver_task 中直接调用会
        // 占死 cooperative worker,abort 无法穿透。block_in_place 允许 runtime
        // 在阻塞期间调度其他 task(非完整 eventfd/AsyncCancel 方案)。
        let submit_result =
            tokio::task::block_in_place(|| ring.submitter().submit_and_wait(total_sqes));
        if submit_result.is_err() {
            // 提交失败：通知所有 inflight 请求。
            // 此处不回收 buf_idx——submit_and_wait 失败时部分 SQE 可能已被内核
            // 消费并仍在处理,贸然释放会导致复用竞争。索引泄漏是安全的(不被复用)。
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
                    // 内核写操作已完成(成功或失败),fixed buffer 不再被内核引用,
                    // 此时回收索引是安全点(所有权在调用方 mark_submitted 时转移给 driver)。
                    pool.free(write_req.buf_idx);
                    let _ = write_req.done.send(result);
                }
                InflightReq::Read(read_req) => {
                    let result = if r < 0 {
                        Err(DownloadError::Io(std::io::Error::from_raw_os_error(-r)))
                    } else {
                        let bytes_read = r as usize;
                        // 从 fixed buffer 复制到 Vec 返回
                        let buf = &buffers[read_req.buf_idx];
                        // SAFETY:
                        // - buf.as_ptr() 返回该 fixed buffer 对齐后的起始地址,
                        //   指向 buffers[buf_idx] 内核已读取的内存区域(ReadFixed 完成)。
                        // - bytes_read = cqe.result(),是内核实际写入的字节数,满足
                        //   0 <= bytes_read <= read_req.read_len(由 ReadFixed 语义保证)。
                        //   read_len 已校验 <= buf.len()(submit_read 中 actual_len = min),
                        //   故 bytes_read <= buf.len(),切片范围在 buffer 有效区间内。
                        // - 此处只读不写,且 driver task 是 buffers 的唯一消费者此刻
                        //   (该 buf_idx 已被 submit_read 分配并独占,完成后才 free),
                        //   不存在并发别名引用。
                        let src = unsafe { std::slice::from_raw_parts(buf.as_ptr(), bytes_read) };
                        Ok(src.to_vec())
                    };
                    // 数据已从 fixed buffer 拷出(to_vec),内核读操作已完成,
                    // 回收索引(必须在 to_vec 之后,确保数据已脱离 buffer)。
                    pool.free(read_req.buf_idx);
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

        // 若 CQE 缺失,通知剩余 inflight 请求。
        // 不回收 buf_idx——缺失的 CQE 对应的内核 op 可能仍在处理,泄漏是安全的。
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
    /// 委托给 `BufferIndexPool`(跨平台纯逻辑)。返回的索引保证落在 `buffers` 内,
    /// 当所有 buffer 都被占用时返回 None。
    fn alloc_buffer_index(&self) -> Option<usize> {
        self.pool.alloc()
    }
}

/// fixed buffer 索引分配池(跨平台纯逻辑,不依赖 io_uring 内核)。
///
/// 位图语义: `0` = 空闲, `1` = 已占用。超出 `buffer_count` 的高位在构造时
/// 预置为 `1`,防止分配到越界索引。`alloc`/`free` 均为原子操作,可在多线程
/// 并发调用(driver task 在 CQE 回收、调用方在提交前分配)。
///
/// # 索引所有权模型
///
/// 索引所有权在命令发送给 driver 时由调用方转移给 driver_task;driver_task
/// 在收到该 op 的 CQE(成功或错误)时回收索引。这避免了取消路径下调用方
/// 提前释放索引、而内核仍持有引用该 buffer 的 in-flight SQE 所致的数据竞争。
/// driver 异常退出时未回收的索引将泄漏(不被复用,安全)。
#[cfg(any(test, target_os = "linux"))]
struct BufferIndexPool {
    bitmap: Box<[AtomicU64]>,
    buffer_count: usize,
}

#[cfg(any(test, target_os = "linux"))]
impl BufferIndexPool {
    fn new(buffer_count: usize) -> Self {
        Self {
            bitmap: build_buffer_bitmap(buffer_count),
            buffer_count,
        }
    }

    /// 原子分配一个空闲索引,全部占用时返回 None。
    fn alloc(&self) -> Option<usize> {
        bitmap_alloc_first_free(&self.bitmap, self.buffer_count)
    }

    /// 释放索引使其可被重新分配。idx 越界时静默忽略。
    fn free(&self, idx: usize) {
        if idx >= self.buffer_count {
            return;
        }
        let word_idx = idx / 64;
        let bit = idx % 64;
        // word_idx < bitmap.len() 由 build_buffer_bitmap 保证(div_ceil),
        // 且 idx < buffer_count 时 word_idx 一定在位图范围内。
        self.bitmap[word_idx].fetch_and(!(1u64 << bit), Ordering::Relaxed);
    }

    /// 重置所有槽位为空闲,一次性回收异常路径泄漏的索引。
    ///
    /// 将 bitmap 所有 word 置 0(包括 `build_buffer_bitmap` 预占的越界高位)。
    /// reset 后 `alloc` 内部的 `idx >= buffer_count` 兜底校验仍防止越界分配,
    /// 故高位被清零不会导致分配到越界索引。
    ///
    /// 用于 driver Shutdown 和 IoUringStorage::drop 路径,回收 submit_and_wait
    /// 失败、CQE 缺失、driver panic/abort 等异常路径泄漏的索引。幂等:重复
    /// 调用不产生副作用,保证 drop + shutdown 双路径都调用 reset 不相互干扰。
    fn reset(&self) {
        for word in &self.bitmap {
            word.store(0, Ordering::Relaxed);
        }
    }
}

/// RAII 守卫,管理 io_uring fixed buffer 索引在调用方一侧的生命周期。
///
/// # 所有权模型(审计 H-07)
///
/// `submit_read`/`submit_write` 在 `alloc_buffer_index()` 后创建本守卫。守卫持
/// 有 `submitted` 标志,描述索引所有权是否已转移给 driver_task:
///
/// - `submitted == false`(命令尚未入队):Drop 时回收索引。覆盖 `alloc` 后、
///   `reserve().await` 等待 channel 容量期间的外层 `select!` 取消,以及
///   `mark_submitted` 前的 `?` 提前返回——此刻 driver 未收到命令、内核无
///   in-flight op,回收安全。
/// - `submitted == true`(permit 已拿到且命令已/将 `permit.send`):Drop 时
///   **不** 回收索引。driver 在 CQE 完成时回收;若调用方在 **入队之后** 被
///   cancel,driver 仍可能持有 in-flight SQE,提前释放会与内核竞争 buffer。
///
/// **H-07 关键顺序**:必须先 `cmd_tx.reserve().await` 拿到 permit,再
/// `mark_submitted`,再同步 `permit.send`。禁止在 await send 前 mark——
/// 旧实现在 `send().await` 等待容量时取消会泄漏 16 槽 fixed buffer。
///
/// 正常完成路径:driver 在 CQE 完成时已回收索引;守卫 `submitted == true`,
/// Drop 也不再回收——无双重释放。
///
/// 对称于 IOCP 路径的 `PendingWriteCancelGuard`(见 `iocp.rs`)。
#[cfg(any(test, target_os = "linux"))]
struct IoUringBufferGuard {
    pool: std::sync::Arc<BufferIndexPool>,
    buf_idx: usize,
    /// `true` = 命令已/将发送给 driver,索引所有权已转移,Drop 不回收。
    submitted: bool,
}

#[cfg(any(test, target_os = "linux"))]
impl IoUringBufferGuard {
    fn new(pool: std::sync::Arc<BufferIndexPool>, buf_idx: usize) -> Self {
        Self {
            pool,
            buf_idx,
            submitted: false,
        }
    }

    /// 标记命令即将经 `permit.send` 入队,索引所有权转移给 driver_task。
    ///
    /// 审计 H-07:仅在 `cmd_tx.reserve().await` **成功之后**调用。此时 channel
    /// 已预留容量,`permit.send` 为同步非 await,取消窗口不再覆盖"等容量"。
    fn mark_submitted(&mut self) {
        self.submitted = true;
    }
}

#[cfg(any(test, target_os = "linux"))]
impl Drop for IoUringBufferGuard {
    fn drop(&mut self) {
        if self.submitted {
            // 所有权已转移给 driver_task:driver 在 CQE 完成时回收索引。
            // 此处不回收,避免与 driver 残留的内核 in-flight op 竞争同一 buffer
            // (新操作复用索引并写入会覆盖正在被内核读写的 buffer)。
            // driver 异常退出(done_rx 返回 Err)时索引将泄漏——泄漏是安全的
            // (索引保持占用态不会被复用),仅损失一个 buffer 槽位。
            tracing::debug!(
                buf_idx = self.buf_idx,
                "IoUringBufferGuard: 索引所有权已转移给 driver,Drop 不回收"
            );
            return;
        }
        // 命令尚未发送给 driver(alloc 后、mark_submitted 前的提前返回/取消):
        // 内核无 in-flight op,回收索引安全。
        self.pool.free(self.buf_idx);
    }
}

/// 构造 fixed buffer 分配位图(Box<[AtomicU64]>)。
///
/// M-01: 多字位图。字数 = `div_ceil(buffer_count, 64)`。
/// 位图语义: `0` = 空闲, `1` = 已占用。最后一个 word 中超出 `buffer_count`
/// 的高位预置为 `1`(已占用),防止 `alloc` 分配到越界索引——与 `iocp.rs`
/// 的 `free_bitmap` 初始化模式一致(见 `CompletionSlots::new`)。
#[cfg(any(test, target_os = "linux"))]
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
#[cfg(any(test, target_os = "linux"))]
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

    // SAFETY 前置条件:`ptr()` 中的 unsafe `as_mut_ptr().add(self.offset)` 依赖
    //   offset < align(否则 .add 超出对齐块边界)与 offset + size <= storage_len
    //   (否则 .add(size) 越界写入)成立。此处用 `assert!`(release 也检查)兜底,
    //   把潜在的越界 UB 转为可隔离的 panic。debug_assert 在 release 下被移除会丢失
    //   这层保护。调用方(aligned_alloc 自身计算 offset)已保证不变式,assert 仅作
    //   soundness 兜底:若未来重构破坏了 offset 计算,此处会立刻 panic 而非静默 UB。
    assert!(
        offset < align,
        "对齐偏移 offset 必须 < align(SAFETY:ptr().add(offset) 前置条件)"
    );
    assert!(
        offset + size <= storage_len,
        "SAFETY:offset + size 必须 <= storage_len,否则 ptr().add(size) 越界写入"
    );

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
            write_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
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
        // 索引分配池由调用方(提交前 alloc)与 driver task(CQE 完成时回收)共享。
        let pool = std::sync::Arc::new(BufferIndexPool::new(buffer_count));
        let driver_pool = std::sync::Arc::clone(&pool);

        let driver_join = tokio::spawn(async move {
            driver_task(ring, cmd_rx, driver_buffers, driver_pool).await;
        });

        self.ring = Some(std::sync::Arc::new(IoUringHandle {
            cmd_tx,
            driver_join: std::sync::Mutex::new(Some(driver_join)),
            buffers: buffers_arc,
            pool,
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
        // RAII 守卫:在命令发送前发生 `?` 提前返回或外层 select! 取消时回收索引。
        // 命令发送后(submitted=true)所有权转移给 driver,Drop 不再回收——
        // driver 在 CQE 完成时回收,避免与内核 in-flight op 竞争同一 buffer。
        let mut guard = IoUringBufferGuard::new(ring_handle.pool.clone(), buf_idx);

        let (done_tx, done_rx) = tokio::sync::oneshot::channel();

        // 审计 H-07:先 reserve 拿到 channel 容量,再 mark_submitted + 同步 send。
        // reserve 等待期间若被外层 select! 取消,guard 仍 unsubmitted,Drop 回收槽位。
        //
        // tokio 1.52 的 `Permit::send(self, value: T)` 返回 `()` 而非 `Result`
        // (与 `Sender::send` 不同)。Permit 已保证容量,reserve 成功即代表
        // channel 未关闭(否则 reserve 返回 Err),故 send 不可能因 channel 关闭
        // 失败——若调用者担心 reserve 后 driver task 退出,可在 done_rx.await
        // 时捕获 RecvError(driver 退出时 drop done_tx,await 返回 Err)。
        let permit =
            ring_handle.cmd_tx.reserve().await.map_err(|_| {
                DownloadError::Io(std::io::Error::other("io_uring driver task 已关闭"))
            })?;
        guard.mark_submitted();
        permit.send(DriverCmd::Read(ReadReq {
            offset,
            read_len,
            fd,
            buf_idx,
            done: done_tx,
        }));

        let read_result: Vec<u8> = done_rx
            .await
            .map_err(|_| DownloadError::Io(std::io::Error::other("io_uring 读取完成通知丢失")))??;

        // driver 已在 CQE 完成时回收 buf_idx(数据已从 fixed buffer 拷出),
        // 此处无需释放;guard(submitted=true)的 Drop 也不会回收,无双重释放。

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
    ///
    /// 使用 `FALLOC_FL_KEEP_SIZE`:只预留物理块,不扩展逻辑 EOF。这样
    /// RMW 尾块的 ftruncate 收尾仍以用户写入末尾为准,不会把 allocate 的
    /// 预分配尺寸误当成"已有数据大小"而拒绝截掉 padding。
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
            // - mode=FALLOC_FL_KEEP_SIZE、offset=0、len=size 均为合法的 fallocate 参数
            // - 内核负责实际的磁盘空间预分配,不破坏 Rust 内存安全
            let ret =
                unsafe { libc::fallocate(fd, libc::FALLOC_FL_KEEP_SIZE, 0, size as libc::off_t) };
            if ret != 0 {
                return Err(DownloadError::Io(std::io::Error::last_os_error()));
            }
            Ok(())
        })
        .await
        .map_err(|e| DownloadError::Io(std::io::Error::other(e.to_string())))?
    }

    /// F-05-1: 把文件截断到 `target_size`(若当前更大)。
    ///
    /// io_uring O_DIRECT 非对齐尾块的 RMW 慢速路径会把内部 buffer 填充到对齐
    /// 边界再整块写回,导致文件 EOF 被扩展到对齐边界(例:写 10001 字节,
    /// padded 到 12288 整块写回,EOF 变 12288)。此方法在 padded write 完成后
    /// 调用 `ftruncate(target_size)` 把文件截回真实大小。
    ///
    /// 调用方 MUST 传入 `max(write_前文件大小, offset + data.len())`,禁止把
    /// 已有更大文件截小——否则 concurrent fast write / allocate 扩展的内容
    /// 会被 RMW 尾截断冲掉(F-05-3)。
    ///
    /// 用 `spawn_blocking` 调 ftruncate(同步 syscall),与 close 中 sync_all
    /// 的处理一致,避免阻塞 tokio 工作线程。
    #[cfg(target_os = "linux")]
    async fn truncate_to(&self, target_size: u64) -> DownloadResult<()> {
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
            // SAFETY:
            // - fd 来自合法打开的 Arc<File>,file_guard 在调用期间保持 Arc 存活
            // - ftruncate 的 length 参数为 i64(off_t);target_size <= i64::MAX
            //   由调用方保证(write_at 中 target = max(size_before, offset+len),
            //   均 usize 范围内,且 allocate 入口已校验 size <= i64::MAX)
            // - ftruncate 把文件截断到指定长度,不破坏 Rust 内存安全
            let ret = unsafe { libc::ftruncate(fd, target_size as libc::off_t) };
            if ret != 0 {
                return Err(DownloadError::Io(std::io::Error::last_os_error()));
            }
            Ok(())
        })
        .await
        .map_err(|e| DownloadError::Io(std::io::Error::other(e.to_string())))?
    }
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
        // RAII 守卫:在命令发送前发生 `?` 提前返回或外层 select! 取消时回收索引。
        // 命令发送后(submitted=true)所有权转移给 driver,Drop 不再回收--
        // driver 在 CQE 完成时回收,避免与内核 in-flight op 竞争同一 buffer。
        let mut guard = IoUringBufferGuard::new(ring_handle.pool.clone(), buf_idx);

        // 将数据复制到 fixed buffer（必须在发送请求前完成，
        // 因为 driver task 会直接引用 buffer 地址构造 SQE）
        let buf = &ring_handle.buffers[buf_idx];
        // SAFETY:
        // - alloc_buffer_index 通过原子位图 CAS 保证同一时刻只有一个操作使用该
        //   buffer 索引(独占);该独占期从 alloc 持续到 driver CQE 回收,此刻
        //   (发送前)无其他别名引用 buffers[buf_idx] 的数据区,可变访问安全。
        // - len 已由上方 validate_fixed_buffer_write_len(len, buffer_len) 校验
        //   len <= buffer_len(= buf.len()),from_raw_parts_mut 切片范围在 buffer 有效区间内。
        // - buf.ptr() 返回对齐后的起始裸指针(UnsafeCell 内部可变性合法化),指向
        //   buffers[buf_idx] 的堆数据,len 字节在 buffer 内存范围内。
        let dst = unsafe { std::slice::from_raw_parts_mut(buf.ptr(), len) };
        dst.copy_from_slice(data);

        let (done_tx, done_rx) = tokio::sync::oneshot::channel();

        // 审计 H-07:先 reserve 拿到 channel 容量,再 mark_submitted + 同步 send。
        // 数据已拷入 fixed buffer;若在 reserve 等待时取消,guard Drop 回收槽位。
        //
        // tokio 1.52 的 `Permit::send(self, value: T)` 返回 `()` 而非 `Result`
        // (详见 submit_read 同段注释)。reserve 成功即保证 channel 未关闭。
        let permit =
            ring_handle.cmd_tx.reserve().await.map_err(|_| {
                DownloadError::Io(std::io::Error::other("io_uring driver task 已关闭"))
            })?;
        guard.mark_submitted();
        permit.send(DriverCmd::Write(WriteReq {
            offset,
            len,
            fd,
            buf_idx,
            done: done_tx,
        }));

        let result = done_rx
            .await
            .map_err(|_| DownloadError::Io(std::io::Error::other("io_uring 写入完成通知丢失")))?;

        // driver 已在 CQE 完成时回收 buf_idx,此处无需释放;
        // guard(submitted=true)的 Drop 也不会回收,无双重释放。
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
///   2. IoUringHandle 的 Arc 引用计数不会归零,索引分配池 / channel
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

        // F-04: abort + 等待循环后 reset pool,回收 driver 异常路径
        // (submit_and_wait 失败、CQE 缺失、driver panic/abort)泄漏的索引。
        // 若 driver 已正常退出(Shutdown 分支),reset 是幂等的二次调用,无副作用。
        handle.pool.reset();

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
                            // 快速路径:已对齐,直接写入。
                            //
                            // F-05-3 修复:此前 fast write 不持 write_lock,与并发 RMW
                            // 落同一对齐块时,RMW 的"读"读到 fast write 之前的旧数据,
                            // 然后"写回"覆盖 fast write 的结果,产生 lost-update。
                            // 单次 submit_write 对内核原子,但 RMW 是读-改-写非原子序列,
                            // 需与 fast write 互斥。fast write 也持 write_lock,与 RMW
                            // 共享同一临界区,确保同块的 fast write 与 RMW 串行执行。
                            let _fast_guard = self.write_lock.lock().await;
                            validate_fixed_buffer_write_len(_data.len(), self.config.buffer_size)?;
                            return self.submit_write(_offset, &_data).await;
                        }

                        // 慢速路径:非对齐写入,采用读-改-写(RMW)
                        //
                        // 此前实现用全零缓冲区填充对齐边界后整块写入,导致 padding 区
                        // 的零覆盖邻近已写数据(并发写同一段时相互破坏)并撑大文件。
                        // 改为:先读回对齐块现有内容,仅覆盖用户数据区间,再整块写回。
                        // 这样 padding 区保留的是文件真实旧数据,而非零。
                        //
                        // B1 修复:RMW 是非原子序列(读块→改→写块),两个并发 RMW 落
                        // 同一对齐块会产生 lost-update(A 读→B 读→A 写→B 写,B 覆盖 A)。
                        // 用 write_lock 串行化整个 RMW 临界区,持锁从 submit_read 到
                        // submit_write 完成。
                        let _rmw_guard = self.write_lock.lock().await;
                        // F-05-3:写前记录逻辑大小。padded write 之后只能截掉"本次
                        // 扩展出的 padding",绝不能把写前已有内容(含 concurrent
                        // fast write / 更大 allocate 结果)截小。
                        let size_before = self.file_size().await.unwrap_or(0);
                        let aligned_offset = _offset & !align_mask;
                        let front_pad = (_offset - aligned_offset) as usize;
                        let total_len = front_pad + _data.len();
                        let padded_len = ((total_len as u64 + align_mask) & !align_mask) as usize;

                        // 如果填充后超过 fixed buffer 大小,无法走 io_uring
                        if padded_len > self.config.buffer_size {
                            return Err(DownloadError::Io(std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                format!(
                                    "io_uring 非对齐写入填充后 {padded_len} 字节超过 fixed buffer 大小 {}",
                                    self.config.buffer_size
                                ),
                            )));
                        }

                        let mut buf = vec![0u8; padded_len];
                        // F-05-2:读回对齐块现有内容,错误必须传播(不静默零填充)。
                        //
                        // submit_read 可能因真实 I/O 错误(EIO、驱动关闭、通道断开)
                        // 失败;若静默吞掉,padding 区保持零,写回时会用零覆盖文件中
                        // 已有真实数据(数据破坏),且调用方误以为写入成功。
                        //
                        // 例外:文件 EOF 之后的扩展区(尚未分配)读会返回短读或
                        // EINVAL/ENODATA,此时 padding 区保持零是正确的(文件真实
                        // 状态即全零),不应作为错误阻断写入。submit_read 内部对
                        // ReadFixed 的短读(bytes_read < read_len)只复制实际读取
                        // 字节,不返回错误——短读属正常 EOF 行为;只有 cqe.result() < 0
                        // (真实 I/O 错误)才返回 Err。故此处传播错误不会误捕 EOF
                        // 短读,安全。
                        self.submit_read(aligned_offset, &mut buf).await?;
                        // 仅覆盖用户数据区间,padding 区保留读回的真实旧数据
                        buf[front_pad..front_pad + _data.len()].copy_from_slice(&_data);

                        validate_fixed_buffer_write_len(padded_len, self.config.buffer_size)?;
                        let written = self.submit_write(aligned_offset, &buf).await?;
                        // F-05-1 + F-05-3:若 padded write 把 EOF 撑过用户写入末尾,
                        // 只截掉 padding 扩展。target = max(写前大小, 用户写入末尾),
                        // 避免 concurrent fast write 的数据被 truncate 冲掉。
                        // truncate 必须在 RMW 临界区内执行。
                        let write_end = _offset + _data.len() as u64;
                        if written.saturating_sub(front_pad) >= _data.len() {
                            let padded_end = aligned_offset + padded_len as u64;
                            let target = size_before.max(write_end);
                            if padded_end > target {
                                self.truncate_to(target).await?;
                            }
                        }
                        // 锁释放在此(drop _rmw_guard)——RMW 临界区结束(truncate 已在锁内完成)。
                        drop(_rmw_guard);
                        // submit_write 返回整块写入量(含 padding),调用方期望用户数据字节数。
                        // O_DIRECT 对齐写入通常一次完成(padded 全部写入),此时用户数据完整
                        // 覆盖,返回 _data.len()。若短写未覆盖全部用户数据,按实际覆盖量
                        // 返回(扣除 front_pad 偏移)。
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
                            // F-05-3:fast write 持 write_lock,与并发 RMW 互斥。
                            // 详见 write_at fast path 注释。
                            let _fast_guard = self.write_lock.lock().await;
                            validate_fixed_buffer_write_len(_data.len(), self.config.buffer_size)?;
                            return self.submit_write(_offset, _data).await;
                        }

                        // 慢速路径读-改-写(RMW):先读回对齐块现有内容,仅覆盖用户数据
                        // 区间,再整块写回。padding 区保留文件真实旧数据,而非零,避免
                        // 零覆盖邻近已写数据并撑大文件。详见 write_at 慢速路径注释。
                        //
                        // B1 修复:RMW 非原子,并发同块 lost-update。write_lock 串行化
                        // 整个 RMW 临界区(submit_read → 改 → submit_write)。
                        let _rmw_guard = self.write_lock.lock().await;
                        // F-05-3:写前记录逻辑大小,详见 write_at 注释。
                        let size_before = self.file_size().await.unwrap_or(0);
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

                        let mut buf = vec![0u8; padded_len];
                        // F-05-2:RMW 读错误传播(不静默零填充),详见 write_at 注释。
                        self.submit_read(aligned_offset, &mut buf).await?;
                        buf[front_pad..front_pad + _data.len()].copy_from_slice(_data);

                        validate_fixed_buffer_write_len(padded_len, self.config.buffer_size)?;
                        let written = self.submit_write(aligned_offset, &buf).await?;
                        // F-05-1 + F-05-3:只截 padding,不截写前已有内容。详见 write_at。
                        let write_end = _offset + _data.len() as u64;
                        if written.saturating_sub(front_pad) >= _data.len() {
                            let padded_end = aligned_offset + padded_len as u64;
                            let target = size_before.max(write_end);
                            if padded_end > target {
                                self.truncate_to(target).await?;
                            }
                        }
                        drop(_rmw_guard);
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
                        // F-15:文件 fsync 后 sync 父目录(防断电丢目录项)。
                        let path = self.file_path.clone();
                        if let Some(file) = self.file_fd.clone() {
                            tokio::task::spawn_blocking(move || {
                                file.sync_all().map_err(DownloadError::Io)?;
                                crate::sync_parent_dir(&path).map_err(DownloadError::Io)?;
                                Ok::<(), DownloadError>(())
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
        assert_eq!(config.buffer_size, 256 * 1024);
        assert_eq!(config.buffer_count, 16);
        assert!(!config.sqpoll);
        assert_eq!(config.sqpoll_idle_ms, 1000);
    }

    #[test]
    fn test_default_config_buffer_total() {
        let config = IoUringConfig::default();
        let total = config.buffer_size * config.buffer_count;
        assert_eq!(total, 4 * 1024 * 1024, "默认总 buffer 应为 4MB");
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

    /// 回归测试:默认 IoUringConfig.buffer_size 必须能容纳引擎的 WRITE_BATCH_BYTES,
    /// 否则 Linux 默认 io_uring 后端的对齐快速路径会拒绝 256KB 写入批。
    /// 历史缺陷:buffer_size 曾为 64KB < WRITE_BATCH_BYTES 256KB,导致
    /// validate_fixed_buffer_write_len 必然返回 InvalidInput 错误。
    #[test]
    fn test_default_buffer_size_covers_write_batch_bytes() {
        let config = IoUringConfig::default();
        let batch = tachyon_core::config::WRITE_BATCH_BYTES;
        assert!(
            config.buffer_size >= batch,
            "默认 buffer_size {} 必须 >= WRITE_BATCH_BYTES {},\
             否则引擎产出的写入批会被 io_uring 对齐快速路径拒绝",
            config.buffer_size,
            batch
        );
        // 等于 batch 大小时应通过校验(对齐快速路径的边界)
        validate_fixed_buffer_write_len(batch, config.buffer_size)
            .expect("默认 buffer_size 应能容纳等于 WRITE_BATCH_BYTES 的对齐写入");
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
            write_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
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

        // 默认 buffer_size 256KB 也应是 4096 的倍数
        let default_size = 256 * 1024usize;
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

    /// SAFETY 回归:`aligned_alloc` 的 offset 不变式必须对所有 (size, align) 组合成立。
    ///
    /// 背景:`ptr()` 的 unsafe `as_mut_ptr().add(self.offset)` 依赖两个前置条件:
    ///   - offset < align(否则 .add 越出对齐块)
    ///   - offset + size <= storage_len(否则 .add(size) 越界写入 → UB)
    /// 旧实现用 `debug_assert` 保护,release 下被移除会丢失这层 soundness 检查,导致
    /// 若未来 offset 计算被破坏则静默 UB。升级为 `assert!` 后,本测试验证断言在各组合下
    /// 不触发(即不变式成立),同时通过 `as_ptr()`(内部调用 `ptr()`)走一遍 unsafe 路径。
    #[cfg(target_os = "linux")]
    #[test]
    fn test_aligned_alloc_offset_invariants_hold() {
        // (size, align) 多样组合覆盖:对齐边界、非对齐、最小/大尺寸、不同对齐值
        let cases: [(usize, usize); 6] = [
            (1, 512),
            (100, 512),
            (512, 512),
            (513, 512),
            (1024, 4096),
            (256 * 1024, 4096),
        ];
        for (size, align) in cases {
            let storage_len = size + (align - 1);
            let buf = aligned_alloc(size, align);
            // 白盒读取 offset(同模块测试可访问私有字段),验证 SAFETY 前置条件
            assert!(
                buf.offset < align,
                "size={size} align={align}: offset={} 必须 < align(SAFETY:ptr().add(offset) 前置条件)",
                buf.offset
            );
            assert!(
                buf.offset + size <= storage_len,
                "size={size} align={align}: offset+size={} 必须 <= storage_len={storage_len}(SAFETY:ptr().add(size) 越界保护)",
                buf.offset + size
            );
            assert_eq!(buf.len(), size, "逻辑长度应保持调用方请求的大小");
            // 走一遍 unsafe 路径(as_ptr 内部调用 ptr()->as_mut_ptr().add(offset)),
            // 验证地址对齐且非空(assert! 已保证不越界,此处验证语义正确性)
            let addr = buf.as_ptr() as usize;
            assert!(addr.is_multiple_of(align), "暴露的指针应按 align 对齐");
        }
    }

    /// 回归测试: buffer_count=16 时,首次分配必须返回 idx=0 而非 16。
    ///
    /// 此前 bug: `alloc_buffer_index` 直接用 `current.trailing_zeros()` 取
    /// 第一个置位(已占用)位,与"找第一个空闲位"语义相反。
    /// 初始化 used_mask = (!0u64) << 16 = 0xFFFFFFFFFFFF0000 时,
    /// trailing_zeros 返回 16, 导致 buffers[16] 越界 panic。
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

    /// FIX-02: 守卫未提交(submitted=false)时 Drop 应回收索引(命令从未发送给 driver)。
    ///
    /// 模拟 alloc 成功后、mark_submitted 前的提前返回或取消路径:此刻 driver 未收到
    /// 命令、内核无 in-flight op,索引可安全回收。
    #[test]
    fn test_guard_not_submitted_frees_on_drop() {
        let pool = std::sync::Arc::new(BufferIndexPool::new(4));
        let idx = pool.alloc().expect("首次分配应成功");
        assert_eq!(idx, 0);
        {
            let _guard = IoUringBufferGuard::new(pool.clone(), idx);
            // drop: submitted=false -> 回收 idx
        }
        // 回收后应能再次分配到 idx=0
        let idx2 = pool.alloc().expect("回收后应可重新分配");
        assert_eq!(idx2, 0, "未提交守卫 Drop 后索引应被回收");
    }

    /// FIX-02: 守卫已提交(submitted=true)时 Drop 不应回收索引——driver 在 CQE 完成时回收。
    ///
    /// 模拟取消路径:命令已发送给 driver,调用方 future 被外层 select! drop,但 driver
    /// 仍持有引用该 buffer 的 in-flight SQE。若守卫此时回收,新操作会复用该索引并
    /// copy_from_slice 覆盖正在被内核读写的 buffer,造成数据竞争。故应泄漏(安全)。
    #[test]
    fn test_guard_submitted_does_not_free_on_drop() {
        let pool = std::sync::Arc::new(BufferIndexPool::new(4));
        let idx = pool.alloc().expect("首次分配应成功");
        assert_eq!(idx, 0);
        {
            let mut guard = IoUringBufferGuard::new(pool.clone(), idx);
            guard.mark_submitted();
            // drop: submitted=true -> 不回收(模拟 driver 仍持有 in-flight op)
        }
        // idx=0 仍被占用,下次分配应返回 idx=1
        let idx2 = pool.alloc().expect("应分配到下一个索引");
        assert_eq!(idx2, 1, "已提交守卫 Drop 后索引不应被回收");
    }

    /// FIX-02: 分配/释放互斥性——占用的索引在释放前不会被再次分配。
    #[test]
    fn test_guard_alloc_exclusivity() {
        let pool = std::sync::Arc::new(BufferIndexPool::new(3));
        let a = pool.alloc().unwrap();
        let b = pool.alloc().unwrap();
        let c = pool.alloc().unwrap();
        assert_eq!([a, b, c], [0, 1, 2]);
        assert!(pool.alloc().is_none(), "占满后应返回 None");
        pool.free(b);
        assert_eq!(pool.alloc(), Some(1), "释放 b 后应重新分配到 1");
    }

    /// FIX-02: 模拟完整生命周期(取消 + driver CQE 回收):
    ///
    /// 1. 调用方分配 A 并 mark_submitted(所有权转移给 driver);
    /// 2. 调用方 future 被取消(drop guard)——driver 仍持有 in-flight op;
    /// 3. 新操作 alloc:不得返回 A(driver 仍在用),应返回 B;
    /// 4. driver 完成 CQE,pool.free(A) 回收 A;
    /// 5. 下次 alloc 应返回 A(已回收)。
    ///
    /// 旧实现在第 2 步就回收了 A,导致第 3 步复用 A 并与内核 in-flight op 竞争。
    #[test]
    fn test_guard_submitted_blocks_realloc_until_driver_frees() {
        let pool = std::sync::Arc::new(BufferIndexPool::new(2));
        // 调用方分配 A 并提交给 driver
        let a = pool.alloc().expect("分配 A");
        assert_eq!(a, 0);
        let mut guard_a = IoUringBufferGuard::new(pool.clone(), a);
        guard_a.mark_submitted(); // 所有权转移给 driver

        // 模拟调用方 future 被取消(drop guard_a)——driver 仍持有 in-flight op
        drop(guard_a);

        // 新操作分配:不得返回 A(driver 仍在用),应返回 B=1
        let b = pool.alloc().expect("分配 B");
        assert_eq!(b, 1, "driver 持有的索引不应被复用");

        // driver 完成 CQE,回收 A
        pool.free(a);

        // 下次分配应返回 A=0
        let next = pool.alloc().expect("A 回收后应可分配");
        assert_eq!(next, 0, "driver CQE 回收后索引应可复用");
    }

    /// FIX-02: 模拟 driver 在 SQE 入队失败时回收索引(内核不会处理该 op)。
    ///
    /// 调用方已 mark_submitted(不会回收),故 driver 必须在 push 失败分支回收,
    /// 否则索引永久泄漏。此测试验证 BufferIndexPool::free 可被 driver 侧调用回收。
    #[test]
    fn test_guard_driver_reclaim_on_push_failure() {
        let pool = std::sync::Arc::new(BufferIndexPool::new(2));
        let a = pool.alloc().expect("分配 A");
        // 调用方已 mark_submitted,模拟 driver 侧 SQE push 失败:driver 回收索引
        pool.free(a);
        // 回收后应能再次分配到 A
        assert_eq!(pool.alloc(), Some(0), "driver push 失败回收后索引应可复用");
    }

    /// 审计 H-07:reserve 等待期间取消(未 mark)必须回收——与 mark 前 Drop 同构。
    #[test]
    fn test_h07_cancel_before_mark_does_not_exhaust_pool() {
        let pool = std::sync::Arc::new(BufferIndexPool::new(4));
        // 模拟 4 次"alloc → 等 channel 时 cancel(未 mark) → Drop"
        for _ in 0..4 {
            let idx = pool.alloc().expect("未提交取消后槽位应可复用");
            let guard = IoUringBufferGuard::new(pool.clone(), idx);
            drop(guard); // submitted=false
        }
        // 池应仍可满配 4 个
        let mut held = Vec::new();
        for i in 0..4 {
            held.push(
                pool.alloc()
                    .unwrap_or_else(|| panic!("第 {i} 次 alloc 失败,池被错误耗尽")),
            );
        }
        assert!(pool.alloc().is_none());
        for idx in held {
            pool.free(idx);
        }
    }

    /// B1 并发 RMW 数据完整性测试。
    ///
    /// io_uring 非对齐写入走读-改-写(RMW):读回对齐块→覆盖用户区→整块写回,
    /// 这是非原子序列。修复前 IoUringStorage 无 write_lock,两个并发 RMW 落
    /// 同一对齐块会产生 lost-update(A 读→B 读→A 写→B 写,B 覆盖 A 的修改)。
    /// 本测试并发写同一对齐块(4096B)的不同非对齐 offset,验证两者数据都正确落盘。
    ///
    /// 设计:
    /// - 同一 4096B 对齐块(offset 0..4096)内,并发写 offset=10(len=20,0xAA)
    ///   和 offset=100(len=20,0xBB),两者非对齐且落同一块,均走 RMW 慢速路径。
    /// - 多轮迭代提高竞态检出概率(无锁时偶发性丢失)。
    /// - 读回验证:offset=10 处为 0xAA,offset=100 处为 0xBB,互不覆盖。
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn test_iouring_concurrent_rmw_same_block_no_lost_update() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        const ROUNDS: usize = 20;
        for round in 0..ROUNDS {
            let path = dir.path().join(format!("iouring_rmw_{round}.bin"));
            let mut storage = IoUringStorage::new(&path, IoUringConfig::default());
            if storage.init().is_err() {
                eprintln!("skip: io_uring init failed (CI runner kernel may not support io_uring)");
                return;
            }
            storage.allocate(4096).await.expect("预分配应成功");
            let storage = std::sync::Arc::new(storage);

            // 并发写同一对齐块(0..4096)的两个非对齐区间,均触发 RMW 慢速路径
            let s1 = storage.clone();
            let h1 = tokio::spawn(async move {
                let data = Bytes::from(vec![0xAAu8; 20]);
                s1.write_at(10, data).await
            });
            let s2 = storage.clone();
            let h2 = tokio::spawn(async move {
                let data = Bytes::from(vec![0xBBu8; 20]);
                s2.write_at(100, data).await
            });
            let (r1, r2) = tokio::join!(h1, h2);
            r1.expect("task1 join").expect("write_at(10) 应成功");
            r2.expect("task2 join").expect("write_at(100) 应成功");

            // 读回验证两段数据都正确落盘,互不覆盖。
            // read_at 要求 O_DIRECT 对齐(offset/len 均 4096 倍数),
            // 故读整个对齐块后切片检查非对齐区间。
            let mut block = vec![0u8; 4096];
            storage
                .read_at(0, &mut block)
                .await
                .expect("read_at(0) 对齐块");
            let buf_a = &block[10..30];
            assert!(
                buf_a.iter().all(|&b| b == 0xAA),
                "round {round}: offset=10 应为 0xAA,实际 {buf_a:?}(并发 RMW lost-update)"
            );
            let buf_b = &block[100..120];
            assert!(
                buf_b.iter().all(|&b| b == 0xBB),
                "round {round}: offset=100 应为 0xBB,实际 {buf_b:?}(并发 RMW lost-update)"
            );
            storage.close().await.expect("close");
        }
    }

    /// F-05-1: 非对齐尾块写入后文件大小应等于声明大小(无 EOF 扩展)。
    ///
    /// io_uring 的 O_DIRECT 要求 offset/len 按 4096 对齐。当用户写入非对齐尾块
    /// (例:10001 字节,offset=0,len=10001)时,慢速路径 RMW 会把内部 buffer 填充
    /// 到 12288 字节(3 * 4096)再整块写回。O_DIRECT 的整块写入会把文件 EOF
    /// 扩展到 12288,导致文件比用户声明的大小多出 2287 字节的"伪尾"。
    ///
    /// 期望行为:写入完成后,文件大小必须等于用户写入的字节总数(10001),
    /// 不能被 padded write 扩展到对齐边界。实现应在 padded write 完成后调用
    /// `ftruncate(expected_size)` 把文件截断到真实大小。
    ///
    /// 本测试 RED:当前实现无 ftruncate 收尾,文件大小为 12288 而非 10001。
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_iouring_non_aligned_write_does_not_extend_file() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let path = dir.path().join("iouring_eof_extend.bin");

        let mut storage = IoUringStorage::new(&path, IoUringConfig::default());
        if storage.init().is_err() {
            eprintln!("skip: io_uring init failed (CI runner kernel may not support io_uring)");
            return;
        }
        // 预分配 12288 字节(覆盖 padded 写入的最大范围),避免 write 因文件
        // 未分配而失败。allocate 使用 FALLOC_FL_KEEP_SIZE,不撑大逻辑 EOF。
        storage.allocate(12288).await.expect("预分配应成功");

        // 写入 10001 字节(非 4096 对齐):10001 = 2*4096 + 1809
        // RMW 慢速路径会填充到 12288(3*4096)再整块写回
        const EXPECTED_SIZE: usize = 10001;
        let data = Bytes::from(vec![0xABu8; EXPECTED_SIZE]);
        let written = storage.write_at(0, data).await.expect("非对齐写入应成功");
        assert_eq!(
            written, EXPECTED_SIZE,
            "write_at 应返回用户数据字节数(非 padded 长度)"
        );

        storage.sync().await.expect("sync 应成功");

        // 读回文件大小:必须等于 10001,不能是 12288
        let actual_size = storage.file_size().await.expect("file_size 应成功");
        assert_eq!(
            actual_size, EXPECTED_SIZE as u64,
            "非对齐尾块写入后文件大小应等于用户声明大小 {EXPECTED_SIZE},\
             实际 {actual_size}(padded O_DIRECT write 把 EOF 扩展到对齐边界,F-05-1)"
        );

        // 双重确认:用 std::fs::metadata 独立读取,绕过 io_uring 路径
        let metadata_size = std::fs::metadata(&path).expect("metadata 应可读").len();
        assert_eq!(
            metadata_size, EXPECTED_SIZE as u64,
            "std::fs::metadata 报告的文件大小也应为 {EXPECTED_SIZE},\
             实际 {metadata_size}(EOF 被扩展,F-05-1)"
        );

        storage.close().await.expect("close");
    }

    /// F-05-2: RMW 读错误不应被静默吞掉当零填充。
    ///
    /// 当前慢速路径 `let _ = self.submit_read(aligned_offset, &mut buf).await;`
    /// 把 submit_read 的所有错误(EIO、驱动关闭、通道断开)都静默忽略,
    /// 未读回的区间保持零。这有两个风险:
    ///   1. 若 submit_read 因 EIO 失败,而该对齐块在文件中已有真实数据,
    ///      零填充会覆盖用户区间外的既有字节(数据破坏)。
    ///   2. 错误被吞掉,调用方以为写入成功,实际可能损坏数据。
    ///
    /// 期望行为:RMW 读错误应传播为 write_at 的 Err,或至少不覆盖用户区间外
    /// 的既有数据。本测试验证契约:若 submit_read 返回错误,write_at 不应
    /// 静默成功,且用户区间外数据应保持不变。
    ///
    /// 策略:无法直接注入 EIO,但可构造"RMW 必须读回既有数据"的场景——
    /// 先写入对齐块 A(4096 字节,0xCC),再写非对齐尾块覆盖同块的后半段。
    /// 若 RMW 读错误被静默吞掉,读回的 padding 区会是 0 而非 0xCC,
    /// 导致 padding 区被错误地写为零。读回验证 padding 区仍为 0xCC。
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_iouring_rmw_read_error_propagates_not_silently_zeroed() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let path = dir.path().join("iouring_rmw_read_silent.bin");

        let mut storage = IoUringStorage::new(&path, IoUringConfig::default());
        if storage.init().is_err() {
            eprintln!("skip: io_uring init failed (CI runner kernel may not support io_uring)");
            return;
        }
        storage.allocate(8192).await.expect("预分配应成功");

        // 步骤 1:先写满整个 4096 对齐块(块 0)为 0xCC
        let block_a = Bytes::from(vec![0xCCu8; 4096]);
        storage
            .write_at(0, block_a)
            .await
            .expect("对齐写入块 A 应成功");
        storage.sync().await.expect("sync 应成功");

        // 步骤 2:写非对齐尾块到同块的后半段:offset=3000, len=200
        // 此时 RMW 必须读回 offset=0..4096 的内容(应为 0xCC),
        // 覆盖 [3000..3200] 为 0xDD,再整块写回。
        // 若 submit_read 被静默当零填充,padding 区 [0..3000] 会被写成 0
        // 而非保留 0xCC。
        let tail = Bytes::from(vec![0xDDu8; 200]);
        let written = storage
            .write_at(3000, tail)
            .await
            .expect("非对齐尾块 RMW 写入应成功");
        assert_eq!(written, 200, "应返回用户数据字节数");
        storage.sync().await.expect("sync 应成功");

        // 步骤 3:读回整个块,验证 padding 区 [0..3000] 仍为 0xCC,
        // 用户区 [3000..3200] 为 0xDD
        let mut block = vec![0u8; 4096];
        storage
            .read_at(0, &mut block)
            .await
            .expect("read_at 应成功");

        // padding 区(用户区间外):应为 0xCC,不能被静默零填充
        let padding = &block[0..3000];
        let zeroed_count = padding.iter().filter(|&&b| b == 0).count();
        assert_eq!(
            zeroed_count, 0,
            "RMW padding 区(用户区间外)应保留原 0xCC,不能被静默零填充(F-05-2);\
             但 [0..3000) 区间发现 {zeroed_count} 个零字节"
        );
        let cc_count = padding.iter().filter(|&&b| b == 0xCC).count();
        assert_eq!(
            cc_count, 3000,
            "RMW padding 区 [0..3000) 应全部为 0xCC(保留读回的旧数据),\
             实际只有 {cc_count} 个 0xCC(F-05-2: submit_read 错误被静默吞掉)"
        );

        // 用户区:应为 0xDD
        let user = &block[3000..3200];
        assert!(
            user.iter().all(|&b| b == 0xDD),
            "用户数据区间 [3000..3200) 应为 0xDD,实际 {user:?}"
        );

        storage.close().await.expect("close");
    }

    /// F-05-3: fast write(对齐 4KiB)与邻接 RMW(非对齐尾块)落同一对齐块时
    /// 不应产生 lost-update。
    ///
    /// 两处失败模式:
    /// 1. 互斥缺失:对齐快速路径不持 `write_lock` 时,RMW 可能读到 fast write
    ///    之前的旧数据再整块写回,覆盖 fast write 结果。
    /// 2. 错误 truncate:即使已互斥,RMW 在 padded write 后若无条件
    ///    `truncate_to(offset+len)`,会把 concurrent fast write 已扩展到 4096
    ///    的文件截回 30;随后 O_DIRECT 读 4096 在 EOF 之后填零,表现为
    ///    [0..10)=0xAA,[10..30)=0xBB,[30..4096)=0 —— aa_count=0。
    ///
    /// 场景:同一 4096 对齐块(块 0):
    ///   - 并发 A:fast write,offset=0, len=4096, 数据 0xAA(对齐快速路径)
    ///   - 并发 B:RMW,offset=10, len=20, 数据 0xBB(非对齐慢速路径)
    ///
    /// 期望(在 RMW 后于 fast write 的串行顺序下,最常见的 composition):
    /// offset=0..4096 为 0xAA,其中 [10..30] 被 RMW 覆盖为 0xBB。
    /// 若 RMW 先于 fast write,fast write 会整块覆盖 RMW 区间,此时 BB 不保留
    /// ——本测试用多轮提高检出"互斥/截断"类 bug 的概率。
    ///
    /// 参考 `test_iouring_concurrent_rmw_same_block_no_lost_update`(RMW×RMW),
    /// 本测试补 fast×RMW 组合。
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn test_iouring_fast_write_and_rmw_same_block_no_lost_update() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        const ROUNDS: usize = 30;
        for round in 0..ROUNDS {
            let path = dir.path().join(format!("iouring_fast_rmw_{round}.bin"));
            let mut storage = IoUringStorage::new(&path, IoUringConfig::default());
            if storage.init().is_err() {
                eprintln!("skip: io_uring init failed (CI runner kernel may not support io_uring)");
                return;
            }
            storage.allocate(4096).await.expect("预分配应成功");
            let storage = std::sync::Arc::new(storage);

            // 并发写同一 4096 对齐块:
            // - h1:对齐 fast write(offset=0, len=4096, 全 0xAA)
            // - h2:非对齐 RMW(offset=10, len=20, 全 0xBB)
            let s1 = storage.clone();
            let h1 = tokio::spawn(async move {
                let data = Bytes::from(vec![0xAAu8; 4096]);
                s1.write_at(0, data).await
            });
            let s2 = storage.clone();
            let h2 = tokio::spawn(async move {
                let data = Bytes::from(vec![0xBBu8; 20]);
                s2.write_at(10, data).await
            });
            let (r1, r2) = tokio::join!(h1, h2);
            r1.expect("task1 join").expect("fast write_at(0) 应成功");
            r2.expect("task2 join").expect("rmw write_at(10) 应成功");

            // 读回验证:无 lost-update。
            // - 若 fast write 被 RMW 覆盖:[0..10] 为 0, [30..4096] 为 0(BAD)
            // - 若 RMW 被 fast write 覆盖:[10..30] 为 0xAA 而非 0xBB(BAD)
            // - 正确:fast write 的 0xAA 落盘,RMW 的 0xBB 叠加在 [10..30]
            let mut block = vec![0u8; 4096];
            storage
                .read_at(0, &mut block)
                .await
                .expect("read_at(0) 对齐块");

            // 1) fast write 的数据应大面积存在(整个块应基本全是 0xAA,
            //    除被 RMW 覆盖的 [10..30])
            let aa_outside = &block[0..10];
            assert!(
                aa_outside.iter().all(|&b| b == 0xAA),
                "round {round}: [0..10) 应为 0xAA(fast write 应落盘),\
                 实际 {aa_outside:?}(fast write 被 RMW lost-update 覆盖,F-05-3)"
            );
            let aa_tail = &block[30..4096];
            let aa_count = aa_tail.iter().filter(|&&b| b == 0xAA).count();
            assert_eq!(
                aa_count,
                4096 - 30,
                "round {round}: [30..4096) 应全为 0xAA(fast write 应落盘),\
                 实际只有 {aa_count} 个 0xAA(F-05-3: fast write lost-update)"
            );

            // 2) RMW 的数据应叠加在 [10..30]
            let bb = &block[10..30];
            assert!(
                bb.iter().all(|&b| b == 0xBB),
                "round {round}: [10..30) 应为 0xBB(RMW 应落盘),\
                 实际 {bb:?}(RMW 被 fast write lost-update 覆盖,F-05-3)"
            );

            storage.close().await.expect("close");
        }
    }

    // =====================================================================
    // F-04 RED 测试:io_uring driver 异常槽位回收
    //
    // 审计发现:
    // - submit_and_wait 失败(iouring.rs:581-605)不回收 buf_idx
    // - CQE 缺失(iouring.rs:667-685)不回收
    // - driver 退出(iouring.rs:689-691)不回收 inflight
    // - 16 slot 耗尽后 alloc_buffer_index() 返回 None,后端不可用
    // - BufferIndexPool 无 reset() 方法
    //
    // 期望契约:
    // 1. BufferIndexPool 新增 `fn reset(&self)`:bitmap 全部置 0
    // 2. driver task 正常退出(Shutdown)前调 pool.reset()
    // 3. IoUringStorage::Drop 中 abort 后 reset
    // 4. 异常退出仅 tracing::error!,storage 进入 IoUringState::Unavailable 状态
    //
    // RED 状态:reset() 方法尚未实现,以下测试编译失败。
    // =====================================================================

    /// F-04: BufferIndexPool::reset 清空所有槽位,使耗尽的池可重新分配。
    ///
    /// 契约:`reset(&self)` 原子地将 bitmap 所有 word 置 0(包括 build_buffer_bitmap
    /// 预占的越界高位——reset 后高位也被清零,但 alloc 内部仍受 `idx >= buffer_count`
    /// 兜底校验保护,不会越界分配)。
    ///
    /// 场景:16 个 slot 全部分配后池耗尽,reset 后应能再次分配 16 个索引。
    /// 这覆盖 submit_and_wait 失败 / CQE 缺失 / driver 退出三类异常路径泄漏后,
    /// 通过 reset 一次性回收所有槽位的能力。
    ///
    /// 预期失败原因:`BufferIndexPool::reset` 方法不存在,编译错误:
    /// `no method named reset found for struct BufferIndexPool`。
    #[test]
    fn test_buffer_index_pool_reset_clears_all_slots() {
        let pool = BufferIndexPool::new(16);
        // 占满 16 个有效槽
        for i in 0..16 {
            assert_eq!(pool.alloc(), Some(i), "顺序分配 idx {i}");
        }
        assert!(pool.alloc().is_none(), "16 槽占满后应返回 None");

        // reset 清空所有槽位(包括异常路径泄漏的索引)
        pool.reset();

        // reset 后应能再次分配 16 个索引
        for i in 0..16 {
            assert_eq!(
                pool.alloc(),
                Some(i),
                "reset 后应能再次顺序分配 idx {i}(异常槽位回收)"
            );
        }
        assert!(pool.alloc().is_none(), "再次占满后应返回 None");
    }

    /// F-04: reset 多次调用幂等,不 panic,bitmap 仍全 0。
    ///
    /// 契约:reset 是幂等操作——重复调用不产生副作用(不 panic、不 double-free、
    /// 不改变 bitmap 全 0 状态)。这保证 IoUringStorage::drop 和 driver Shutdown
    /// 两条路径都调用 reset 时不会相互干扰。
    ///
    /// 预期失败原因:`BufferIndexPool::reset` 方法不存在,编译错误。
    #[test]
    fn test_pool_reset_idempotent() {
        let pool = BufferIndexPool::new(8);
        // 占满 8 个槽
        for i in 0..8 {
            assert_eq!(pool.alloc(), Some(i));
        }
        assert!(pool.alloc().is_none());

        // 连续 reset 多次(模拟 drop + shutdown 双路径都调用)
        pool.reset();
        pool.reset();
        pool.reset();

        // 仍可分配 8 个索引
        for i in 0..8 {
            assert_eq!(
                pool.alloc(),
                Some(i),
                "多次 reset 后应仍可分配 idx {i}(幂等性)"
            );
        }
        assert!(pool.alloc().is_none(), "8 槽占满后应返回 None");
    }

    /// F-04: driver task 收到 Shutdown 命令正常退出前应调用 pool.reset()。
    ///
    /// 契约:`driver_task` 的 `DriverCmd::Shutdown` 分支在 break 之前调用
    /// `pool.reset()`,使所有因异常路径(submit_and_wait 失败、CQE 缺失、
    /// driver panic)泄漏的 buffer 索引被回收。
    ///
    /// 测试方式:启动真实 driver_task,分配 2 个索引不释放(模拟异常路径泄漏),
    /// 发送 Shutdown,等待 driver 退出,检查 pool 可重新分配到 idx=0(证明 reset 被调用)。
    ///
    /// 预期失败原因:driver_task 未调用 pool.reset(),泄漏的 2 个索引未被回收,
    /// reset 后 pool.alloc() 仍返回 Some(2) 而非 Some(0)。
    ///
    /// 注:此测试需要真实 io_uring 内核支持,仅在 Linux 上编译运行。
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_driver_shutdown_resets_pool() {
        use io_uring::IoUring;
        let ring = IoUring::builder().build(8).expect("构建 io_uring 应成功");
        let buffers: std::sync::Arc<Vec<AlignedBuffer>> =
            std::sync::Arc::new(vec![aligned_alloc(4096, 4096)]);
        let pool = std::sync::Arc::new(BufferIndexPool::new(4));
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<DriverCmd>(8);
        let driver_pool = pool.clone();
        let handle = tokio::spawn(async move {
            driver_task(ring, cmd_rx, buffers, driver_pool).await;
        });

        // 模拟异常路径泄漏:分配 2 个索引不释放(无对应 CQE 回收)
        let leaked_a = pool.alloc().expect("分配 A");
        let leaked_b = pool.alloc().expect("分配 B");
        assert_eq!([leaked_a, leaked_b], [0, 1]);

        // 发送 Shutdown 命令,driver 应在 break 前 reset pool
        cmd_tx
            .send(DriverCmd::Shutdown)
            .await
            .expect("发送 Shutdown 应成功");
        handle.await.expect("driver task join 应成功");

        // driver 退出前应 reset pool,泄漏的索引被回收
        // reset 后应能再次分配到 idx=0(而非 idx=2)
        let next = pool.alloc().expect("driver shutdown reset 后应可重新分配");
        assert_eq!(
            next, 0,
            "driver Shutdown 退出前应调用 pool.reset(),回收泄漏索引;\
             实际分配到 idx={next}(索引未被回收,F-04)"
        );
    }

    /// F-04: IoUringStorage::drop 应在 abort driver task 后调用 pool.reset()。
    ///
    /// 契约:`Drop for IoUringStorage` 在 abort driver task 后,对 pool 调用
    /// `reset()`,确保 driver 异常路径(submit_and_wait 失败、CQE 缺失、driver
    /// panic、abort 强制取消)泄漏的 buffer 索引被回收。这避免 storage 重建时
    /// 复用问题,也让 pool bitmap 反映真实可用状态。
    ///
    /// 测试方式:初始化 storage,clone 出 pool Arc 引用,分配 3 个索引模拟泄漏,
    /// drop storage,检查 pool 可重新分配到 idx=0(证明 reset 被调用)。
    ///
    /// 预期失败原因:Drop 实现未调用 pool.reset(),泄漏的 3 个索引未被回收,
    /// pool.alloc() 返回 Some(3) 而非 Some(0)。
    ///
    /// 注:此测试需要真实 io_uring 内核支持,仅在 Linux 上编译运行。
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_iouring_storage_drop_resets_pool() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let path = dir.path().join("iouring_drop_reset.bin");

        let mut storage = IoUringStorage::new(&path, IoUringConfig::default());
        if storage.init().is_err() {
            eprintln!("skip: io_uring init failed (CI runner kernel may not support io_uring)");
            return;
        }

        // 取出 pool 引用(用于 drop 后检查 bitmap 状态)
        let ring_handle = storage.ring.as_ref().expect("init 后 ring 应存在").clone();
        let pool = ring_handle.pool.clone();

        // 模拟异常路径泄漏:分配 3 个索引不释放(无对应 CQE 回收)
        for i in 0..3 {
            assert_eq!(pool.alloc(), Some(i), "模拟泄漏 idx {i}");
        }

        // drop storage:应 abort driver 并 reset pool
        drop(storage);

        // drop 后 pool 应被 reset,可重新分配到 idx=0
        let next = pool.alloc().expect("drop 后 pool 应被 reset,可重新分配");
        assert_eq!(
            next, 0,
            "IoUringStorage::drop 应调用 pool.reset(),回收所有泄漏索引;\
             实际分配到 idx={next}(索引未被回收,F-04)"
        );
    }

    /// F-04: storage 进入 Unavailable 状态后,write_at 应返回错误(不 panic)。
    ///
    /// 契约:异常退出路径(driver panic、submit_and_wait 反复失败、init 失败)
    /// 将 storage 状态置为 `IoUringState::Unavailable`,后续 `write_at` 在 match
    /// 中显式覆盖 Unavailable 分支,返回 `NotConnected` Io 错误而非 panic 或
    /// 静默成功。这保证后端不可用时调用方得到明确错误,可降级到其他存储后端。
    ///
    /// 注:`IoUringState::Unavailable` 变体已存在(line 113),write_at 的
    /// `_ =>` 分支已返回 NotConnected 错误。此测试作为契约守卫,确保未来
    /// 重构 match 时 Unavailable 路径不退化为 panic/unreachable。
    ///
    /// 预期失败原因:无(此测试当前应 PASS,作为 GREEN 守卫)。若未来实现将
    /// Unavailable 分支改为 panic 或 unreachable,此测试会失败。
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn test_unavailable_state_returns_error_on_write() {
        let storage = IoUringStorage {
            config: IoUringConfig::default(),
            file_path: PathBuf::from("/tmp/iouring_unavailable.bin"),
            file_fd: None,
            state: IoUringState::Unavailable,
            write_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
            ring: None,
        };

        let result = storage.write_at(0, Bytes::from_static(b"test")).await;
        assert!(
            result.is_err(),
            "Unavailable 状态下 write_at 必须返回错误,不能 panic 或静默成功"
        );

        // 错误应为 Io 错误(NotConnected),而非其他类型
        match result.unwrap_err() {
            DownloadError::Io(io_err) => {
                assert!(
                    matches!(io_err.kind(), std::io::ErrorKind::NotConnected)
                        || io_err.to_string().contains("未初始化")
                        || io_err.to_string().contains("Unavailable"),
                    "Unavailable 状态 write_at 应返回 NotConnected 错误,实际: {io_err}"
                );
            }
            other => panic!("应返回 Io 错误,实际: {other}"),
        }
    }

    // ── F-15(父目录 sync)RED 测试 ──
    //
    // 审计 F-15:IoUringStorage::close 仅 fsync 文件本身(sync_all),不 sync
    // 父目录。Unix 断电后文件数据落盘但目录项创建未持久化,文件可能消失。
    // 多文件 torrent 需逐层新目录持久化。
    //
    // 期望:IoUringStorage::close() 末尾(在 sync_all 之后)调用
    //   `crate::sync_parent_dir(&self.path)`(在 spawn_blocking 闭包内)。
    //
    // 本测试为 RED:函数尚不存在 → E0425。实现后,close 内部调用
    // sync_parent_dir,测试断言:
    // 1. close() 不 panic 且返回 Ok
    // 2. close 后父目录可被 std::fs::File::open 打开(metadata 可读)
    // 3. 显式调用 crate::sync_parent_dir(parent) 验证函数已导出(契约验证)

    /// F-15 契约:IoUringStorage::close() MUST 调用 sync_parent_dir 持久化父目录。
    ///
    /// 预期失败原因:`crate::sync_parent_dir` 函数尚不存在,编译失败(E0425)。
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_iouring_close_syncs_parent_directory() {
        use crate::storage::AsyncStorage;
        use std::path::Path;

        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let path = dir.path().join("iouring_f15_parent_sync.bin");
        let parent: &Path = path.parent().expect("文件应有父目录");

        let mut storage = IoUringStorage::new(&path, IoUringConfig::default());
        if storage.init().is_err() {
            eprintln!("skip: io_uring init failed (CI runner kernel may not support io_uring)");
            return;
        }
        storage.allocate(4096).await.expect("预分配应成功");
        storage
            .write_at(
                0,
                Bytes::from_static(b"f15-parent-sync-padded-to-4096-bytes!!"),
            )
            .await
            .expect("write_at 应成功");
        storage
            .close()
            .await
            .expect("close 应成功(含 sync_parent_dir)");

        // 1. close 后父目录 metadata 应可读(目录项存在)
        let parent_meta = std::fs::metadata(parent).expect("close 后父目录 metadata 应可读");
        assert!(parent_meta.is_dir(), "父目录应为目录");

        // 2. 契约验证:sync_parent_dir 已导出且对真实父目录工作
        // (RED hook:函数不存在时此行触发 E0425)
        let sync_result = crate::sync_parent_dir(&path);
        assert!(
            sync_result.is_ok(),
            "sync_parent_dir 对已存在文件的父目录应返回 Ok: {:?}",
            sync_result.err()
        );
    }
}
