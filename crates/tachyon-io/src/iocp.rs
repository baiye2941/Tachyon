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
use std::cell::UnsafeCell;
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
/// `data` 必须由完成 slot 持有到内核完成通知抵达,避免调用方取消
/// `write_at` future 后提前释放传给 `WriteFile` 的缓冲区。
/// 当 `write_at_mut` 调用方在 await 期间保证缓冲区存活时,可置为 `None`。
#[cfg(target_os = "windows")]
struct PendingWrite {
    completion: tokio::sync::oneshot::Sender<DownloadResult<usize>>,
    data: Option<Bytes>,
}

// ── B1: Slot array 完成注册表(无锁,替换原 Mutex<HashMap>) ──────

/// Slot 状态
#[cfg(target_os = "windows")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum SlotState {
    /// 空闲,可分配
    Free = 0,
    /// 已分配并提交 WriteFile,等待完成通知
    Submitted = 1,
}

/// 完成槽:每个槽持有一个 OVERLAPPED(固定地址)+ pending 写入上下文
///
/// OVERLAPPED 结构体的**固定地址**是核心:Windows 内核在完成通知中原样返回
/// 提交时的 OVERLAPPED 指针,因此 `(returned_ptr - base) / sizeof::<Slot>()`
/// 即可恢复 slot 索引,完全无锁、无需 HashMap 查找。
#[cfg(target_os = "windows")]
#[repr(C)]
struct CompletionSlot {
    /// Windows 内核 OVERLAPPED 结构(内核原样返回此字段地址)
    overlapped: UnsafeCell<KernelOverlapped>,
    /// slot 状态(Free/Submitted),原子操作驱动分配/回收
    state: std::sync::atomic::AtomicU8,
    /// pending 写入上下文(仅 state==Submitted 时有效)
    pending: UnsafeCell<Option<PendingWrite>>,
    /// F-15:分配代际计数器。每次 alloc() 复用此 slot 时 fetch_add(1),
    /// 用于消除 CancelGuard 的 TOCTOU 窗口:guard 持有提交时的 expected_generation,
    /// drop 时若 generation 已变,说明 slot 被回收并提交了新 I/O,此时 CancelIoEx
    /// 会误取消无关 I/O,故跳过。state 检查无法覆盖此场景(检查后另一线程 alloc
    /// 并重新置 SUBMITTED,CancelIoEx 仍用旧 overlapped 地址但内核已绑定新 I/O)。
    generation: std::sync::atomic::AtomicU64,
}

#[cfg(target_os = "windows")]
impl CompletionSlot {
    const FREE: u8 = SlotState::Free as u8;
    const SUBMITTED: u8 = SlotState::Submitted as u8;
}

// Safety: CompletionSlot 含两个 UnsafeCell(overlapped + pending),跨线程共享看似违反 Rust 别名规则,
// 但实际访问受 slot 生命周期状态机保护:
// - overlapped:仅在 slot 处于 Free->Submitted 转换时(alloc 后、WriteFile 前)由提交线程写入 reset;
//   提交后到完成前由 Windows 内核独占访问(Internal/InternalHigh 字段);完成后内核不再触碰。
// - pending:仅在 Submitted 状态下,提交线程写入(write_at),poller 单线程读取(complete_pending_write)。
//   state 从 Submitted->Free 的转换是单向且由 poller 单线程执行,配合 Release/Acquire 内存序保证可见性。
// 因此不存在两个线程同时对同一 UnsafeCell 执行写操作的情况。
#[cfg(target_os = "windows")]
unsafe impl Send for CompletionSlot {}
#[cfg(target_os = "windows")]
unsafe impl Sync for CompletionSlot {}

/// 默认 slot 容量:256(覆盖 256 并发分片)
#[cfg(target_os = "windows")]
const IOCP_SLOT_CAPACITY: usize = 256;

/// 无锁完成槽数组 + 位图分配器
#[cfg(target_os = "windows")]
struct CompletionSlots {
    slots: Box<[CompletionSlot]>,
    free_bitmap: Box<[std::sync::atomic::AtomicU64]>,
    base_addr: usize,
    slot_stride: usize,
}

#[cfg(target_os = "windows")]
impl CompletionSlots {
    fn new() -> Self {
        use std::sync::atomic::AtomicU64;
        let slots: Box<[CompletionSlot]> = (0..IOCP_SLOT_CAPACITY)
            .map(|_| CompletionSlot {
                overlapped: UnsafeCell::new(KernelOverlapped::new_for_offset(0)),
                state: std::sync::atomic::AtomicU8::new(CompletionSlot::FREE),
                pending: UnsafeCell::new(None),
                generation: std::sync::atomic::AtomicU64::new(0),
            })
            .collect();
        let bitmap_words = IOCP_SLOT_CAPACITY.div_ceil(64);
        // 位图语义: 0=空闲,1=已占用。最后一个 word 的超出容量的位预置为 1(已占用),
        // 防止 alloc() 分配到越界 slot,与 io_uring 的 used_mask 模式一致。
        let free_bitmap: Box<[AtomicU64]> = (0..bitmap_words)
            .map(|word_idx| {
                let excess = (word_idx as i64 + 1) * 64 - IOCP_SLOT_CAPACITY as i64;
                if excess > 0 {
                    // 最后一个 word: 超出容量的高位标记为已占用
                    AtomicU64::new((!0u64) << (64 - excess as usize))
                } else {
                    AtomicU64::new(0)
                }
            })
            .collect();
        let base_addr = slots.as_ptr() as usize;
        let slot_stride = std::mem::size_of::<CompletionSlot>();
        Self {
            slots,
            free_bitmap,
            base_addr,
            slot_stride,
        }
    }

    fn alloc(&self) -> Option<(usize, u64)> {
        use std::sync::atomic::Ordering;
        for (word_idx, word) in self.free_bitmap.iter().enumerate() {
            let mut current = word.load(Ordering::Relaxed);
            loop {
                if current == u64::MAX {
                    break;
                }
                let bit = (!current).trailing_zeros() as usize;
                let global_slot = word_idx * 64 + bit;
                if global_slot >= IOCP_SLOT_CAPACITY {
                    return None;
                }
                let new_val = current | (1u64 << bit);
                match word.compare_exchange_weak(
                    current,
                    new_val,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        self.slots[global_slot]
                            .state
                            .store(CompletionSlot::SUBMITTED, Ordering::Release);
                        // F-15:递增代际计数器,返回新 generation 供 CancelGuard 比对。
                        let generation = self.slots[global_slot]
                            .generation
                            .fetch_add(1, Ordering::AcqRel);
                        return Some((global_slot, generation.wrapping_add(1)));
                    }
                    Err(actual) => current = actual,
                }
            }
        }
        None
    }

    fn release(&self, slot_index: usize) {
        use std::sync::atomic::Ordering;
        let word_idx = slot_index / 64;
        let bit = slot_index % 64;
        let mask = !(1u64 << bit);
        let mut current = self.free_bitmap[word_idx].load(Ordering::Relaxed);
        loop {
            let new_val = current & mask;
            match self.free_bitmap[word_idx].compare_exchange_weak(
                current,
                new_val,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    /// 从 OVERLAPPED 指针恢复 slot 索引
    ///
    /// # Safety
    ///
    /// 调用者必须保证 `ptr` 来自 `self.slots` 数组中某个 slot 的
    /// `overlapped.get()` 返回值,或者是一个明显不在范围内的值(函数
    /// 会校验范围并返回 None)。传入非法地址可能触发未定义行为。
    unsafe fn slot_index_from_ptr(&self, ptr: usize) -> Option<usize> {
        if ptr < self.base_addr || self.slot_stride == 0 {
            return None;
        }
        let offset = ptr - self.base_addr;
        let index = offset / self.slot_stride;
        if !offset.is_multiple_of(self.slot_stride) || index >= IOCP_SLOT_CAPACITY {
            return None;
        }
        let slot_ptr = self.base_addr + index * self.slot_stride;
        if slot_ptr != ptr {
            return None;
        }
        Some(index)
    }

    fn pending_count(&self) -> usize {
        use std::sync::atomic::Ordering;
        self.free_bitmap
            .iter()
            .map(|w| w.load(Ordering::Relaxed).count_ones() as usize)
            .sum()
    }
}

#[cfg(target_os = "windows")]
struct PendingWriteCancelGuard {
    file_handle: usize,
    slot_index: usize,
    slots: std::sync::Arc<CompletionSlots>,
    /// 提交 I/O 时捕获的 slot generation,用于 drop 时检测 slot 是否已被回收重用。
    expected_generation: u64,
    armed: bool,
}

#[cfg(target_os = "windows")]
impl PendingWriteCancelGuard {
    fn new(
        file_handle: windows_sys::Win32::Foundation::HANDLE,
        slot_index: usize,
        slots: std::sync::Arc<CompletionSlots>,
        expected_generation: u64,
    ) -> Self {
        Self {
            file_handle: file_handle as usize,
            slot_index,
            slots,
            expected_generation,
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
        // F-15:双重检查消除 TOCTOU 窗口。
        //
        // 1) generation 检查:若 slot generation 已变,说明 poller 完成后另一线程
        //    已 alloc 复用此 slot 并提交新 I/O,此时 overlapped 地址已绑定新 I/O,
        //    CancelIoEx 会误取消无关操作,故必须跳过。state 检查无法覆盖此场景
        //    (检查后另一线程 alloc 并重新置 SUBMITTED,此时 state 恢复但 I/O 已换)。
        //
        // 2) state 检查:generation 未变但 state 已转 Free,说明 poller 刚完成,
        //    overlapped 内核侧已释放,CancelIoEx 将返回 ERROR_NOT_FOUND(无害)。
        let current_gen = self.slots.slots[self.slot_index]
            .generation
            .load(std::sync::atomic::Ordering::Acquire);
        if current_gen != self.expected_generation {
            tracing::debug!(
                slot = self.slot_index,
                expected_gen = self.expected_generation,
                actual_gen = current_gen,
                "CancelGuard: slot 已被回收重用(generation 不匹配),跳过 CancelIoEx 避免误取消"
            );
            return;
        }
        let current_state = self.slots.slots[self.slot_index]
            .state
            .load(std::sync::atomic::Ordering::Acquire);
        if current_state != CompletionSlot::SUBMITTED {
            tracing::debug!(
                slot = self.slot_index,
                state = current_state,
                "CancelGuard: slot 已被 poller 回收,跳过 CancelIoEx"
            );
            return;
        }
        let file_handle = self.file_handle as windows_sys::Win32::Foundation::HANDLE;
        let overlapped_ptr = self.slots.slots[self.slot_index].overlapped.get()
            as *mut windows_sys::Win32::System::IO::OVERLAPPED;
        // Safety: file_handle 来自仍存活的 IoCpStorage 文件句柄;overlapped_ptr 指向 slot_index
        // 对应的 slot.overlapped。上方 generation + state 双重检查已确认:
        // (1) slot 仍属于本次提交(generation 未变);
        // (2) state 仍为 Submitted(poller 未完成回收)。
        // 即 overlapped 仍绑定我们提交的 I/O。CancelIoEx 只请求取消该 pending I/O,
        // 不释放 OVERLAPPED 或缓冲区,内核保证线程安全。
        // 残留窗口:generation/state 检查通过到 CancelIoEx 之间,poller 可能完成并回收、
        // 另一线程 alloc 复用——但此时 CancelIoEx 返回 ERROR_NOT_FOUND,下方过滤忽略。
        let ok = unsafe { windows_sys::Win32::System::IO::CancelIoEx(file_handle, overlapped_ptr) };
        if ok == 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(windows_sys::Win32::Foundation::ERROR_NOT_FOUND as i32) {
                tracing::warn!(slot = self.slot_index, error = %err, "取消 IOCP pending write 失败");
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
}

// Safety: KernelOverlapped 是提交给 Windows 内核的 POD 状态块,实际内存由
// CompletionSlots 数组固定,只在完成通知抵达后由 poller 线程处理。
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
    /// 无锁完成槽数组(替换原 Mutex<HashMap> + OverlappedPool)
    ///
    /// 256 个固定 slot,每个 slot 持有 OVERLAPPED(固定地址)+ pending 上下文。
    /// Windows 内核完成通知原样返回 OVERLAPPED 地址,通过指针算术恢复 slot 索引,
    /// 完全消除原 parking_lot::Mutex 的串行化瓶颈。
    slots: std::sync::Arc<CompletionSlots>,
    /// NO_BUFFERING 模式下非对齐写入的 buffered fallback 句柄
    fallback: std::sync::Mutex<Option<std::fs::File>>,
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
            slots: std::sync::Arc::new(CompletionSlots::new()),
            fallback: std::sync::Mutex::new(None),
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

    /// NO_BUFFERING 模式扇区对齐常量
    const IOCP_SECTOR_SIZE: u64 = 512;

    /// 惰性获取 buffered fallback 句柄,用于非对齐 I/O
    ///
    /// NO_BUFFERING 要求所有 I/O 偏移和长度按扇区大小对齐。
    /// 不满足对齐要求的写入/读取通过此 fallback 句柄走缓冲 I/O 路径。
    #[cfg(target_os = "windows")]
    fn get_or_init_fallback(&self) -> DownloadResult<std::fs::File> {
        // fast path: fallback 已初始化
        {
            let guard = self.fallback.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref f) = *guard {
                return f.try_clone().map_err(DownloadError::Io);
            }
        }
        // slow path: 惰性打开 buffered 句柄
        use std::os::windows::fs::OpenOptionsExt;
        const SHARE: u32 = 0x00000001 | 0x00000002 | 0x00000004; // READ|WRITE|DELETE
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(SHARE)
            .open(&self.path)
            .map_err(DownloadError::Io)?;
        let mut guard = self.fallback.lock().unwrap_or_else(|e| e.into_inner());
        // 双检:另一线程可能已初始化
        if let Some(ref existing) = *guard {
            return existing.try_clone().map_err(DownloadError::Io);
        }
        let result = file.try_clone().map_err(DownloadError::Io)?;
        *guard = Some(file);
        Ok(result)
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

    /// 提交 IOCP 写入请求。
    ///
    /// `data_ptr`/`data_len` 描述待写入缓冲区，`keep_alive` 在需要时持有缓冲区所有权
    /// 以保证内核完成通知到达前内存有效。调用 `write_at_mut` 时由调用方通过
    /// `&mut BytesMut` 生命周期保证有效性，因此 `keep_alive` 可为 `None`。
    #[cfg(target_os = "windows")]
    async fn submit_iocp_write(
        &self,
        offset: u64,
        data_ptr: usize,
        data_len: usize,
        keep_alive: Option<Bytes>,
    ) -> DownloadResult<usize> {
        use std::os::windows::io::AsRawHandle;
        use std::sync::atomic::Ordering;

        if data_len > u32::MAX as usize {
            return Err(DownloadError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("IOCP 单次写入长度 {data_len} 超过 u32 最大值"),
            )));
        }

        if self.state != IoCpState::Ready {
            return Err(DownloadError::Io(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "IOCP 存储引擎未初始化",
            )));
        }

        let (slot_index, generation) = self.slots.alloc().ok_or_else(|| {
            DownloadError::Io(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "IOCP slot 容量耗尽(256 并发 pending 写入上限)",
            ))
        })?;

        let slot = &self.slots.slots[slot_index];
        // Safety: slot 刚由 alloc() 分配(state=Submitted),此线程是唯一持有者,
        // 内核尚未接触此 OVERLAPPED(WriteFile 未调用)。reset 写入 offset 字段安全。
        unsafe {
            (*slot.overlapped.get()).reset(offset);
        }
        let ov_ptr = slot.overlapped.get() as *mut windows_sys::Win32::System::IO::OVERLAPPED;

        let file = self.file.as_ref().ok_or_else(|| {
            DownloadError::Io(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "IOCP 文件句柄未初始化",
            ))
        })?;
        let file_handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
        let mut bytes_written: u32 = 0;
        let (tx, rx) = tokio::sync::oneshot::channel();

        // Safety: slot.state 已由 alloc() 置为 Submitted(Release 序),此处 Acquire 序
        // 保证可见性。write_at 提交方是唯一写入者,poller 在完成事件到达后才读取。
        unsafe {
            *slot.pending.get() = Some(PendingWrite {
                completion: tx,
                data: keep_alive,
            });
        }

        // SAFETY: file_handle 来自 self.file(合法 File 句柄,as_raw_handle 转换);
        // data_ptr 指向有效缓冲区,keep_alive 或调用方生命周期保证其在内核完成前有效;
        // data_len 已验证不溢出 u32; bytes_written 为合法输出变量;ov_ptr 来自 slot.overlapped.get(),
        // slot 刚由 alloc() 分配,OVERLAPPED 生命周期覆盖 WriteFile 调用期间。
        let write_ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::WriteFile(
                file_handle,
                data_ptr as *const u8,
                data_len as u32,
                &mut bytes_written,
                ov_ptr,
            )
        };

        if write_ok != 0 {
            tracing::debug!(bytes = bytes_written, "IOCP write_at 同步完成");
            // Safety: 同步完成时 poller 不会收到完成通知(FILE_SKIP_COMPLETION_PORT_ON_SUCCESS),
            // slot 仍处于 Submitted,由本路径安全回收 pending。
            unsafe {
                let _ = (*slot.pending.get()).take();
            }
            slot.state.store(CompletionSlot::FREE, Ordering::Release);
            self.slots.release(slot_index);
            return Ok(bytes_written as usize);
        }

        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(windows_sys::Win32::Foundation::ERROR_IO_PENDING as i32) {
            // Safety: WriteFile 提交失败,内核不会发送完成通知,slot 仍处于 Submitted,
            // 由本路径安全回收 pending。
            unsafe {
                let _ = (*slot.pending.get()).take();
            }
            slot.state.store(CompletionSlot::FREE, Ordering::Release);
            self.slots.release(slot_index);
            return Err(map_writefile_submission_error(err));
        }

        let mut cancel_guard =
            PendingWriteCancelGuard::new(file_handle, slot_index, self.slots.clone(), generation);
        let completion = rx.await.map_err(|_| {
            DownloadError::Io(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "IOCP 完成通知通道关闭",
            ))
        });
        cancel_guard.disarm();
        completion?
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

        // FILE_FLAG_OVERLAPPED | FILE_FLAG_SEQUENTIAL_SCAN | FILE_FLAG_NO_BUFFERING
        // Safety:
        // - OVERLAPPED: 使 WriteFile/ReadFile 变为异步,通过 IOCP 完成端口通知。
        // - SEQUENTIAL_SCAN: 提示内核使用顺序预读策略。
        // - NO_BUFFERING: 绕过 Windows Page Cache,写入直接到达磁盘。
        //   约束:所有通过主句柄的 I/O 操作的偏移和长度必须按扇区大小对齐
        //   (通常 512 字节,推荐 4096 字节)。非对齐的写入/读取自动路由到
        //   惰性初始化的 buffered fallback 句柄(见 get_or_init_fallback)。
        const OVERLAPPED_SEQUENTIAL_NOBUF: u32 = 0x40000000 | 0x08000000 | 0x20000000;

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(OVERLAPPED_SEQUENTIAL_NOBUF)
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
        let slots = self.slots.clone();
        // 通过 usize 传递句柄到线程(*mut c_void 不实现 Send)
        let port_raw = port_handle as usize;

        // 启动 IOCP 轮询线程
        let poller = match std::thread::Builder::new()
            .name("iocp-poller".into())
            .spawn(move || {
                // Safety: port_raw 来自成功的 CreateIoCompletionPort,转换回 HANDLE 安全
                let port = port_raw as windows_sys::Win32::Foundation::HANDLE;
                Self::poller_loop(port, &shutdown_flag, &slots);
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
        slots: &std::sync::Arc<CompletionSlots>,
    ) {
        use windows_sys::Win32::System::IO::OVERLAPPED_ENTRY;

        // Safety: OVERLAPPED_ENTRY 是 POD 类型,全零初始化有效
        let mut entries: [OVERLAPPED_ENTRY; 16] = unsafe { std::mem::zeroed() };
        let mut num_entries: u32 = 0;

        loop {
            if shutdown.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }

            // SAFETY: port 是通过 CreateIoCompletionPort 成功创建的合法 IOCP 句柄;
            // entries 数组在栈上分配,生命周期覆盖调用期间;
            // num_entries 是合法的输出指针;超时 100ms 和 alertable=false 为合法参数。
            let ok = unsafe {
                windows_sys::Win32::System::IO::GetQueuedCompletionStatusEx(
                    port,
                    entries.as_mut_ptr(),
                    entries.len() as u32,
                    &mut num_entries,
                    100,
                    0,
                )
            };

            if ok != 0 && num_entries > 0 {
                tracing::debug!(count = num_entries, "IOCP 完成事件");
                for entry in &entries[..num_entries as usize] {
                    let overlapped_ptr = entry.lpOverlapped as usize;
                    let bytes = entry.dwNumberOfBytesTransferred as usize;
                    let status = entry.Internal as i32;
                    Self::complete_pending_write(slots.as_ref(), overlapped_ptr, bytes, status);
                }
            }
        }

        tracing::debug!("IOCP 轮询线程退出");
    }

    /// 处理单个完成事件:恢复 slot 索引,读取 pending,发送结果,回收 slot
    fn complete_pending_write(
        slots: &CompletionSlots,
        overlapped_ptr: usize,
        bytes: usize,
        status: i32,
    ) -> bool {
        use std::sync::atomic::Ordering;

        // SAFETY: slot_index_from_ptr 是 unsafe fn,要求传入的 ptr 来自 slot 数组内的
        // overlapped 地址。此处的 overlapped_ptr 来自 IOCP 完成事件的
        // OVERLAPPED_ENTRY::lpOverlapped,由 Windows 内核原样返回提交时的地址,
        // 而提交时的地址正是 slot.overlapped.get(),在 slot 数组范围内。
        let slot_index = match unsafe { slots.slot_index_from_ptr(overlapped_ptr) } {
            Some(idx) => idx,
            None => {
                // 防御性回退:slot_index_from_ptr 依赖地址算术,极端情况下(如内核返回
                // 被修改的指针、内存损坏)可能计算失败。遍历 slot 数组线性搜索匹配的
                // OVERLAPPED 指针,避免因地址计算失败导致 slot 永久泄漏(bitmap 位无法释放)。
                let mut found: Option<usize> = None;
                for i in 0..IOCP_SLOT_CAPACITY {
                    if slots.slots[i].overlapped.get() as usize == overlapped_ptr {
                        found = Some(i);
                        break;
                    }
                }
                match found {
                    Some(idx) => {
                        tracing::warn!(
                            ptr = overlapped_ptr,
                            slot = idx,
                            "IOCP slot_index_from_ptr 计算失败,线性扫描找到匹配 slot"
                        );
                        idx
                    }
                    None => {
                        // ptr 不属于任何 slot:不是我们提交的 I/O(可能是外部代码向同一
                        // IOCP port 投递的完成事件)。不释放任何 bitmap 位是正确的。
                        tracing::error!(
                            ptr = overlapped_ptr,
                            "IOCP 完成事件指针不在 slot 数组范围内,无法恢复 slot 索引"
                        );
                        return false;
                    }
                }
            }
        };

        let slot = &slots.slots[slot_index];
        let current_state = slot.state.load(Ordering::Acquire);
        if current_state != CompletionSlot::SUBMITTED {
            tracing::warn!(
                slot = slot_index,
                state = current_state,
                "IOCP 完成 slot 状态非 Submitted,跳过"
            );
            return false;
        }

        // Safety: slot.state 已校验为 Submitted(Acquire 序),poller 是唯一将 state
        // 从 Submitted->Free 的执行者,此处 take 与 write_at 提交方的写入无并发别名。
        let pending = unsafe { (*slot.pending.get()).take() };
        let result = if status == 0 {
            Ok(bytes)
        } else {
            Err(map_ntstatus_error(status))
        };
        slot.state.store(CompletionSlot::FREE, Ordering::Release);
        slots.release(slot_index);

        if let Some(PendingWrite { completion, data }) = pending {
            let _ = completion.send(result);
            drop(data);
        }
        true
    }

    fn pending_count(&self) -> usize {
        self.slots.pending_count()
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
                    "IOCP pending I/O 未在超时内完成,泄漏 slots 以避免释放内核仍可能使用的缓冲区"
                );
                std::mem::forget(self.slots.clone());
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
    /// 流程(B1 无锁 slot array):
    /// 1. 从 CompletionSlots 分配一个空闲 slot(原子 CAS,无锁)
    /// 2. 在 slot 中存储 pending 上下文(oneshot Sender + data)
    /// 3. 用 slot 的 OVERLAPPED 地址提交 WriteFile
    /// 4. 同步完成:直接回收 slot 返回;异步完成:poller 通过指针算术恢复 slot 索引
    fn write_at(
        &self,
        offset: u64,
        data: Bytes,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
        Box::pin(async move {
            // 状态校验:fallback 路径会绕过 submit_iocp_write 的 state 检查,需在此前置。
            if self.state != IoCpState::Ready {
                return Err(DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "IOCP 存储引擎未初始化",
                )));
            }
            // NO_BUFFERING 三重对齐要求(任一不满足都会返回 ERROR_INVALID_PARAMETER):
            // 1. 文件偏移按扇区对齐
            // 2. 写入长度按扇区对齐
            // 3. 缓冲区指针(内存地址)按扇区对齐 — 堆分配通常仅 16B 对齐,
            //    bytes::Bytes 内部 Vec<u8> 不保证 512B 对齐,需显式校验。
            let buf_addr = data.as_ptr() as usize as u64;
            let needs_fallback = !offset.is_multiple_of(Self::IOCP_SECTOR_SIZE)
                || !(data.len() as u64).is_multiple_of(Self::IOCP_SECTOR_SIZE)
                || !buf_addr.is_multiple_of(Self::IOCP_SECTOR_SIZE);
            if needs_fallback {
                let fallback_file = self.get_or_init_fallback()?;
                return tokio::task::spawn_blocking(move || {
                    use std::os::windows::fs::FileExt;
                    fallback_file
                        .seek_write(&data, offset)
                        .map_err(DownloadError::Io)
                })
                .await
                .map_err(|e| DownloadError::Io(e.into()))?;
            }

            let data_ptr = data.as_ptr() as usize;
            let data_len = data.len();
            self.submit_iocp_write(offset, data_ptr, data_len, Some(data))
                .await
        })
    }

    fn read_at<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
        Box::pin(async move {
            use std::os::windows::fs::FileExt;

            // 状态校验:fallback 路径会绕过 IOCP 主句柄的 state 检查,需在此前置。
            if self.state != IoCpState::Ready {
                return Err(DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "IOCP 存储引擎未初始化",
                )));
            }
            // NO_BUFFERING 主句柄要求 offset/length/缓冲区指针 三者均按扇区对齐。
            // 内部 owned_buf 是 Vec<u8>(堆分配,通常仅 16B 对齐),无法保证扇区对齐;
            // 因此 IOCP 主句柄不适合通用读取,统一路由到 buffered fallback 句柄,
            // 它无对齐限制且能正确读到最新数据(主句柄写入已 flush 到文件系统)。
            let fallback_file = self.get_or_init_fallback()?;
            let buf_len = buf.len();
            let mut owned_buf = vec![0u8; buf_len];
            let (n, owned_buf) = tokio::task::spawn_blocking(move || {
                let n = fallback_file.seek_read(&mut owned_buf, offset)?;
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
                .map_err(|e| DownloadError::Io(e.into()))??;
            // flush fallback 句柄(若已初始化),保证缓冲数据落盘
            let fallback_file = {
                let guard = self.fallback.lock().unwrap_or_else(|e| e.into_inner());
                guard.as_ref().and_then(|f| f.try_clone().ok())
            };
            if let Some(f) = fallback_file {
                tokio::task::spawn_blocking(move || f.sync_data().map_err(DownloadError::Io))
                    .await
                    .map_err(|e| DownloadError::Io(e.into()))??;
            }
            Ok(())
        })
    }

    fn allocate(&self, size: u64) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        Box::pin(async move {
            let file = self.clone_ready_file()?;
            tokio::task::spawn_blocking(move || {
                // 先 set_len 扩展文件逻辑大小(EOF)
                file.set_len(size).map_err(DownloadError::Io)?;
                // 使用 SetFileInformationByHandle(FileAllocationInfo) 真正预分配物理磁盘块,
                // 避免稀疏文件仅扩展逻辑大小而不分配空间。
                #[cfg(target_os = "windows")]
                {
                    use std::os::windows::io::AsRawHandle;
                    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
                    let info = windows_sys::Win32::Storage::FileSystem::FILE_ALLOCATION_INFO {
                        AllocationSize: size as i64,
                    };
                    // Safety:
                    // - handle 来自合法的 File 句柄(通过 clone_ready_file 获取)
                    // - info 指针指向有效的 FILE_ALLOCATION_INFO 结构
                    // - FileAllocationInfo 是 Windows 定义的标准信息类
                    // - 失败时通过 last_os_error 返回错误,不破坏文件已有状态
                    let result = unsafe {
                        windows_sys::Win32::Storage::FileSystem::SetFileInformationByHandle(
                            handle,
                            windows_sys::Win32::Storage::FileSystem::FileAllocationInfo,
                            &info as *const _ as *const std::ffi::c_void,
                            std::mem::size_of::<
                                windows_sys::Win32::Storage::FileSystem::FILE_ALLOCATION_INFO,
                            >() as u32,
                        )
                    };
                    if result == 0 {
                        return Err(DownloadError::Io(std::io::Error::last_os_error()));
                    }
                }
                // 尝试 SetFileValidData 跳过零填充(需要 SE_MANAGE_VOLUME_NAME 权限)
                // 失败时静默回退(文件已通过 set_len 正确扩展,只是较慢)
                // 注意:成功时文件扩展区域包含磁盘残留数据(非零填充),
                // 但下载数据会立即覆盖,安全风险极低。
                #[cfg(target_os = "windows")]
                {
                    use std::os::windows::io::AsRawHandle;
                    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
                    // Safety:
                    // - handle 来自合法的 File 句柄(通过 clone_ready_file 获取)
                    // - size 由调用方传入,来自文件元数据的合法大小值
                    // - 内核保证:失败时不影响文件已有状态
                    let result = unsafe {
                        windows_sys::Win32::Storage::FileSystem::SetFileValidData(
                            handle,
                            size as i64,
                        )
                    };
                    if result == 0 {
                        // SetFileValidData 失败(通常因权限不足),静默回退
                        tracing::debug!(
                            size,
                            "SetFileValidData 失败(需 SE_MANAGE_VOLUME_NAME),回退到零填充模式"
                        );
                    }
                }
                Ok(())
            })
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

    /// P1-05: IOCP 覆盖 write_at_mut
    ///
    /// 直接读取 `BytesMut` 内部缓冲区，避免 `freeze()` 的原子引用计数操作。
    /// 对于短写场景，调用方 `write_all_at_mut` 会根据返回值 `advance` 并循环补写。
    fn write_at_mut<'a>(
        &'a self,
        offset: u64,
        data: &'a mut bytes::BytesMut,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
        Box::pin(async move {
            // 状态校验:fallback 路径会绕过 submit_iocp_write 的 state 检查,需在此前置。
            if self.state != IoCpState::Ready {
                return Err(DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "IOCP 存储引擎未初始化",
                )));
            }
            // NO_BUFFERING 三重对齐要求(同 write_at):offset/length/buffer pointer
            let buf_addr = data.as_ptr() as usize as u64;
            let needs_fallback = !offset.is_multiple_of(Self::IOCP_SECTOR_SIZE)
                || !(data.len() as u64).is_multiple_of(Self::IOCP_SECTOR_SIZE)
                || !buf_addr.is_multiple_of(Self::IOCP_SECTOR_SIZE);
            if needs_fallback {
                let fallback_file = self.get_or_init_fallback()?;
                let data_ptr = data.as_mut_ptr() as usize;
                let data_len = data.len();
                return tokio::task::spawn_blocking(move || {
                    use std::os::windows::fs::FileExt;
                    // Safety: data_ptr 来自 &mut BytesMut，在 await 返回前始终有效。
                    let slice =
                        unsafe { std::slice::from_raw_parts(data_ptr as *const u8, data_len) };
                    fallback_file
                        .seek_write(slice, offset)
                        .map_err(DownloadError::Io)
                })
                .await
                .map_err(|e| DownloadError::Io(e.into()))?;
            }

            self.submit_iocp_write(offset, data.as_mut_ptr() as usize, data.len(), None)
                .await
        })
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

    /// 获取 Windows 文件分配大小(物理磁盘分配)
    #[cfg(target_os = "windows")]
    fn file_allocation_size(path: &std::path::Path) -> u64 {
        use std::os::windows::io::AsRawHandle;
        let file = std::fs::File::open(path).unwrap();
        let mut info: windows_sys::Win32::Storage::FileSystem::FILE_STANDARD_INFO =
            unsafe { std::mem::zeroed() };
        // Safety:
        // - file 是合法打开的文件句柄
        // - info 指针指向长度为 size_of::<FILE_STANDARD_INFO>() 的可写内存
        // - FileStandardInfo 是 Windows 定义的标准信息类
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandleEx(
                file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE,
                windows_sys::Win32::Storage::FileSystem::FileStandardInfo,
                &mut info as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<windows_sys::Win32::Storage::FileSystem::FILE_STANDARD_INFO>()
                    as u32,
            )
        };
        assert!(ok != 0, "GetFileInformationByHandleEx 失败");
        info.AllocationSize as u64
    }

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

    /// 验证未知 completion 指针(不在 slot 数组范围)不会被误处理。
    #[cfg(target_os = "windows")]
    #[test]
    fn test_iocp_unknown_completion_is_not_owned() {
        let slots = CompletionSlots::new();
        assert!(
            !IoCpStorage::complete_pending_write(&slots, 0xDEAD_BEEF, 0, 0),
            "未知 completion 指针应被忽略"
        );
        assert_eq!(slots.pending_count(), 0);
    }

    /// 验证 slot 命中的 completion 才会移除 pending 并发送写入结果。
    #[cfg(target_os = "windows")]
    #[test]
    fn test_iocp_registered_completion_resolves_pending_write() {
        let slots = std::sync::Arc::new(CompletionSlots::new());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (slot_index, _) = slots.alloc().expect("应有空闲 slot");
        let slot = &slots.slots[slot_index];
        // SAFETY: slot 已由 alloc() 分配(state=Submitted),此线程是唯一持有者,
        // UnsafeCell.get() 返回内部指针,写入 Some(PendingWrite) 不会与
        // poller 线程产生并发别名(poller 仅在完成事件到达后读取 pending)。
        unsafe {
            *slot.pending.get() = Some(PendingWrite {
                completion: tx,
                data: Some(Bytes::from_static(b"abc")),
            });
        }
        let key = slot.overlapped.get() as usize;
        assert_eq!(slots.pending_count(), 1);

        assert!(
            IoCpStorage::complete_pending_write(&slots, key, 3, 0),
            "slot 命中的 completion 应完成 pending write"
        );
        assert_eq!(slots.pending_count(), 0);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let written = rt
            .block_on(rx)
            .expect("completion sender 应发送结果")
            .expect("status=0 应映射为成功");
        assert_eq!(written, 3);
    }

    /// 验证 pending write 的取消 guard 只请求取消,不移除 slot pending。
    #[cfg(target_os = "windows")]
    #[test]
    fn test_iocp_cancel_guard_preserves_pending_slot_entry() {
        let slots = std::sync::Arc::new(CompletionSlots::new());
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let (slot_index, generation) = slots.alloc().expect("应有空闲 slot");
        let slot = &slots.slots[slot_index];
        // SAFETY: 同上,test_iocp_registered_completion_resolves_pending_write。
        unsafe {
            *slot.pending.get() = Some(PendingWrite {
                completion: tx,
                data: Some(Bytes::from_static(b"cancel")),
            });
        }
        {
            let _guard = PendingWriteCancelGuard::new(
                std::ptr::null_mut(),
                slot_index,
                slots.clone(),
                generation,
            );
        }
        assert_eq!(
            slot.state.load(std::sync::atomic::Ordering::Acquire),
            CompletionSlot::SUBMITTED,
            "取消 guard 不能移除 pending"
        );
        assert_eq!(slots.pending_count(), 1);
    }

    /// 验证 CancelGuard 在 slot 已被 poller 回收(state=FREE)时不调用 CancelIoEx。
    ///
    /// D-02 修复:当 poller 已完成 slot 并将 state 设为 FREE 后,CancelGuard::drop
    /// 应检测到 state != SUBMITTED 并跳过 CancelIoEx,防止错误取消新分配的 I/O。
    #[cfg(target_os = "windows")]
    #[test]
    fn test_cancel_guard_skips_when_slot_already_free() {
        let slots = std::sync::Arc::new(CompletionSlots::new());
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let (slot_index, generation) = slots.alloc().expect("应有空闲 slot");
        let slot = &slots.slots[slot_index];
        // SAFETY: slot 已由 alloc() 分配(state=Submitted),此线程是唯一持有者。
        unsafe {
            *slot.pending.get() = Some(PendingWrite {
                completion: tx,
                data: Some(Bytes::from_static(b"racing")),
            });
        }

        // 模拟 poller 已完成该 slot:state→FREE,release bitmap
        unsafe {
            let _ = (*slot.pending.get()).take();
        }
        slot.state
            .store(CompletionSlot::FREE, std::sync::atomic::Ordering::Release);
        slots.release(slot_index);

        // 此时 CancelGuard::drop 应检测到 state=FREE 并跳过 CancelIoEx
        {
            let _guard = PendingWriteCancelGuard::new(
                std::ptr::null_mut(),
                slot_index,
                slots.clone(),
                generation,
            );
        }

        // slot 应仍可重新分配(验证 bitmap 未被破坏)
        let (reused, _) = slots.alloc().expect("回收后的 slot 应可重新分配");
        // 注意:reused 不一定等于 slot_index(其他线程可能分配了其他 slot),
        // 但 pending_count 应从 0 恢复到 1
        assert_eq!(slots.pending_count(), 1);

        // 清理
        slots.slots[reused]
            .state
            .store(CompletionSlot::FREE, std::sync::atomic::Ordering::Release);
        slots.release(reused);
    }

    /// F-15:验证 generation 计数器能在 slot 被回收重用后阻止 CancelIoEx 误取消。
    ///
    /// 场景:线程 A 提交 I/O(捕获 generation=G);poller 完成并 release slot;
    /// 线程 B alloc 复用同一 slot(generation→G+1)提交新 I/O;此时 A 的 CancelGuard
    /// drop 时 generation 不匹配,必须跳过 CancelIoEx(否则会误取消 B 的新 I/O)。
    #[cfg(target_os = "windows")]
    #[test]
    fn test_cancel_guard_skips_when_slot_generation_changed() {
        let slots = std::sync::Arc::new(CompletionSlots::new());
        let (tx, _rx) = tokio::sync::oneshot::channel();
        // 线程 A:提交 I/O,捕获 generation
        let (slot_index, gen_a) = slots.alloc().expect("应有空闲 slot");
        let slot = &slots.slots[slot_index];
        unsafe {
            *slot.pending.get() = Some(PendingWrite {
                completion: tx,
                data: Some(Bytes::from_static(b"first")),
            });
        }

        // poller 完成:state→FREE,release bitmap
        unsafe {
            let _ = (*slot.pending.get()).take();
        }
        slot.state
            .store(CompletionSlot::FREE, std::sync::atomic::Ordering::Release);
        slots.release(slot_index);

        // 线程 B:复用同一 slot,generation 递增
        let (reused, _gen_b) = slots.alloc().expect("回收后应有空闲 slot");
        assert_eq!(
            reused, slot_index,
            "回收后应复用同一 slot 以验证 generation"
        );
        let new_gen = slots.slots[slot_index]
            .generation
            .load(std::sync::atomic::Ordering::Acquire);
        assert_ne!(
            new_gen, gen_a,
            "复用后 generation 必须已递增,否则 generation 检查无效"
        );

        // A 的 CancelGuard drop:generation 不匹配,应跳过 CancelIoEx(不 panic、不误取消)
        {
            let _guard = PendingWriteCancelGuard::new(
                std::ptr::null_mut(),
                slot_index,
                slots.clone(),
                gen_a,
            );
        }

        // 清理
        slots.slots[slot_index]
            .state
            .store(CompletionSlot::FREE, std::sync::atomic::Ordering::Release);
        slots.release(slot_index);
    }

    /// 验证 complete_pending_write 对未知指针不泄漏 bitmap 位。
    ///
    /// D-03 增强:未知指针不对应任何 slot,不应影响 bitmap 分配能力。
    #[cfg(target_os = "windows")]
    #[test]
    fn test_complete_pending_write_unknown_ptr_no_bitmap_leak() {
        let slots = CompletionSlots::new();
        let initial_count = slots.pending_count();

        // 传入一个不在 slot 数组范围内的指针
        assert!(
            !IoCpStorage::complete_pending_write(&slots, 0xDEAD_BEEF, 0, 0),
            "未知完成指针应返回 false"
        );

        // bitmap 不应受影响:pending_count 不变
        assert_eq!(slots.pending_count(), initial_count);

        // 所有 slot 应仍可正常分配
        for _ in 0..IOCP_SLOT_CAPACITY {
            assert!(slots.alloc().is_some(), "所有 slot 应仍可分配");
        }
    }

    // ── B1: CompletionSlots 无锁分配器测试 ──

    #[cfg(target_os = "windows")]
    #[test]
    fn test_completion_slots_alloc_release() {
        let slots = CompletionSlots::new();
        assert_eq!(slots.pending_count(), 0);
        let mut allocated = Vec::new();
        for _ in 0..IOCP_SLOT_CAPACITY {
            let (idx, _) = slots.alloc().expect("应有空闲 slot");
            allocated.push(idx);
        }
        assert_eq!(slots.pending_count(), IOCP_SLOT_CAPACITY);
        assert!(slots.alloc().is_none(), "容量耗尽后 alloc 应返回 None");
        for &idx in allocated.iter().take(IOCP_SLOT_CAPACITY / 2) {
            slots.slots[idx]
                .state
                .store(CompletionSlot::FREE, std::sync::atomic::Ordering::Release);
            slots.release(idx);
        }
        assert_eq!(slots.pending_count(), IOCP_SLOT_CAPACITY / 2);
        let (reused, _) = slots.alloc().expect("回收后应有空闲 slot");
        assert!(allocated[..IOCP_SLOT_CAPACITY / 2].contains(&reused));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_completion_slots_ptr_to_index_roundtrip() {
        let slots = CompletionSlots::new();
        for i in 0..IOCP_SLOT_CAPACITY {
            let ptr = slots.slots[i].overlapped.get() as usize;
            // SAFETY: slot_index_from_ptr 是 unsafe fn,此处的 ptr 来自
            // slots.slots[i].overlapped.get(),是 slot 数组内的合法地址。
            let recovered = unsafe { slots.slot_index_from_ptr(ptr) };
            assert_eq!(recovered, Some(i), "slot {i} 指针应恢复为索引 {i}");
        }
        // SAFETY: 0xDEAD_BEEF 不在 slot 数组范围内,slot_index_from_ptr
        // 内部会校验范围并返回 None,不会产生未定义行为。
        assert_eq!(unsafe { slots.slot_index_from_ptr(0xDEAD_BEEF) }, None);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_completion_slots_concurrent_alloc_no_duplicates() {
        let slots = std::sync::Arc::new(CompletionSlots::new());
        let num_threads = 8;
        let allocs_per_thread = IOCP_SLOT_CAPACITY / num_threads;
        let allocated: Vec<_> = (0..num_threads)
            .map(|_| {
                let slots = slots.clone();
                std::thread::spawn(move || {
                    let mut local = Vec::new();
                    for _ in 0..allocs_per_thread {
                        if let Some((idx, _)) = slots.alloc() {
                            local.push(idx);
                        }
                    }
                    local
                })
            })
            .collect();
        let mut all: Vec<usize> = allocated
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        all.sort();
        let mut deduped = all.clone();
        deduped.dedup();
        assert_eq!(all.len(), deduped.len(), "并发 alloc 不应产生重复 slot");
        assert_eq!(all.len(), num_threads * allocs_per_thread);
        assert_eq!(slots.pending_count(), num_threads * allocs_per_thread);
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn test_iocp_high_concurrency_64_writes() {
        use crate::storage::AsyncStorage;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("iocp_concurrent_64.dat");
        let mut storage = IoCpStorage::new(&path);
        storage.init().expect("IOCP init 应成功");
        storage.allocate(64 * 512).await.unwrap();
        let storage = std::sync::Arc::new(storage);
        let mut handles = Vec::new();
        for index in 0u8..64 {
            let storage = storage.clone();
            handles.push(tokio::spawn(async move {
                let offset = index as u64 * 512;
                let payload = Bytes::from(vec![index; 512]);
                let written = storage.write_at(offset, payload).await?;
                assert_eq!(written, 512);
                Ok::<_, tachyon_core::DownloadError>(())
            }));
        }
        for handle in handles {
            handle.await.unwrap().unwrap();
        }
        for index in 0u8..64 {
            let offset = index as u64 * 512;
            let mut buf = vec![0u8; 512];
            let read = storage.read_at(offset, &mut buf).await.unwrap();
            assert_eq!(read, 512);
            assert!(
                buf.iter().all(|&byte| byte == index),
                "并发写入区域 {offset} 数据不一致"
            );
        }
        storage.sync().await.unwrap();
        storage.close().await.unwrap();
    }

    /// Windows:allocate 后文件物理分配大小应达到请求大小
    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn test_allocate_physical_size_windows() {
        use crate::storage::AsyncStorage;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("iocp_allocate_physical.dat");
        let mut storage = IoCpStorage::new(&path);
        storage.init().expect("IOCP init 应成功");
        storage.allocate(4096).await.unwrap();
        assert_eq!(storage.file_size().await.unwrap(), 4096);
        let alloc = file_allocation_size(&path);
        assert!(
            alloc >= 4096,
            "预分配后文件物理分配大小 {} 小于请求大小 4096",
            alloc
        );
        storage.close().await.unwrap();
    }

    /// 回归测试:offset 与 length 均扇区对齐,但缓冲区指针未对齐时,
    /// 写入仍应通过 fallback 句柄成功,而非抛 ERROR_INVALID_PARAMETER (os error 87)。
    ///
    /// 故障场景(修复前):HTTP 流式下载产生的 Bytes 内部 Vec<u8> 通常仅 16B 对齐,
    /// 当 chunk 长度恰好为 512 倍数(如 16 KiB)且累积偏移也对齐时,会进入
    /// IOCP NO_BUFFERING 主句柄路径,触发内核参数错误。
    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn test_iocp_write_at_unaligned_buffer_ptr_fallback() {
        use crate::storage::AsyncStorage;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("iocp_unaligned_buf.dat");
        let mut storage = IoCpStorage::new(&path);
        storage.init().expect("IOCP init 应成功");
        storage.allocate(8192).await.unwrap();

        // 构造一个长度对齐(1024B = 2 sectors),但起始指针几乎肯定不是 512B 对齐的 Bytes:
        // 先做一个稍大的 Vec,再切片偏移 16 字节,得到对齐于 16B 但绝不是 512B 的指针。
        let mut backing = vec![0u8; 1024 + 16];
        for (i, b) in backing.iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }
        let bytes = bytes::Bytes::from(backing).slice(16..(16 + 1024));
        let buf_addr = bytes.as_ptr() as usize as u64;
        // 该缓冲区: len=1024 对齐,但 ptr 大概率不对齐(若恰好对齐则跳过断言但测试仍验证主路径)
        let len_aligned = (bytes.len() as u64).is_multiple_of(512);
        assert!(len_aligned, "len 必须扇区对齐才能命中故障路径");
        let _ptr_unaligned = !buf_addr.is_multiple_of(512);

        // 即使指针不对齐,写入也应成功(通过 fallback 路径)
        let written = storage
            .write_at(0, bytes.clone())
            .await
            .expect("非对齐指针写入应通过 fallback 成功,而非返回 ERROR_INVALID_PARAMETER");
        assert_eq!(written, 1024);

        // 数据可读回验证
        let mut buf = vec![0u8; 1024];
        let read = storage.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 1024);
        assert_eq!(&buf[..], &bytes[..]);
        storage.close().await.unwrap();
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
