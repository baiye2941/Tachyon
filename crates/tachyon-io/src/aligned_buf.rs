//! 可指定对齐方式的写入缓冲区
//!
//! IOCP（`FILE_FLAG_NO_BUFFERING`）和 WinFile（`FILE_FLAG_NO_BUFFERING`）要求
//! 写入数据的 buffer 指针按扇区大小（512 字节）对齐。标准 `BytesMut::with_capacity`
//! 底层 `Vec<u8>` 仅保证 1 字节对齐，导致 IOCP/WinFile 的对齐快速路径
//! （`submit_iocp_write` / 主句柄 `seek_write`）几乎永不触发，所有写入退化到
//! fallback 串行化路径。
//!
//! `AlignedBuf::new` 用 `std::alloc::alloc(Layout::from_size_align(cap, 512)?)`
//! 分配 512 对齐内存；`AlignedBuf::with_align` 则按调用方请求的对齐值分配。二者均可
//! 通过 `Bytes::from_owner` 零拷贝产出按各自分配对齐的 `Bytes`；`new` 保证满足
//! IOCP/WinFile 的 512 字节指针对齐要求，`with_align` 则须由调用方请求兼容的对齐值。
//!
//! ## 零拷贝 split 设计
//!
//! `BytesMut::split` 是零拷贝的（Arc 共享 + 偏移指针）。`AlignedBuf` 采用相同的
//! Arc 共享策略：`split()` 产出新 `AlignedBuf`（共享底层 `AlignedAlloc`），
//! 不复制数据。split 后的新窗口保持原起始地址的对齐；由 `new` 创建的缓冲区
//! `freeze()` 后产出的 `Bytes` 指针仍为 512 对齐。

use std::alloc::{self, Layout};
use std::io;
use std::ptr::NonNull;
use std::sync::Arc;

use bytes::Bytes;

/// IOCP / WinFile NO_BUFFERING 的扇区对齐要求（字节）
pub const SECTOR_ALIGN: usize = 512;

/// 检查给定 offset 和 data 是否满足 NO_BUFFERING 三向扇区对齐契约。
///
/// IOCP(`FILE_FLAG_NO_BUFFERING`)和 WinFile 要求写入的:
/// - 文件偏移(offset)是扇区大小(512 字节)的倍数
/// - 写入长度(len)是扇区大小的倍数
/// - buffer 指针地址是扇区大小的倍数
///
/// 三者全部满足时返回 true(IOCP 真异步快速路径可触发),
/// 任一不满足返回 false(退化到 fallback 串行化路径)。
///
/// 此函数为纯函数,不依赖任何平台特定类型,可在任意平台测试。
/// IOCP/WinFile 的 `write_at`/`write_at_mut` 内联检查与此函数逻辑完全等价。
///
/// # 等价性说明
///
/// iocp.rs 旧内联检查(`IOCP_SECTOR_SIZE: u64 = 512`):
/// ```text
/// let buf_addr = data.as_ptr() as usize as u64;
/// let needs_fallback = !offset.is_multiple_of(IOCP_SECTOR_SIZE)
///     || !(data.len() as u64).is_multiple_of(IOCP_SECTOR_SIZE)
///     || !buf_addr.is_multiple_of(IOCP_SECTOR_SIZE);
/// ```
/// 由德摩根律 `!A || !B || !C == !(A && B && C)`,故
/// `needs_fallback = !satisfies_no_buffering_alignment(offset, &data)`。
/// 三个操作数均为 `u64`(offset 本身 u64、len as u64、ptr as u64),
/// 本函数对三向均以 `SECTOR_ALIGN as u64` 做模运算,位级等价。
pub fn satisfies_no_buffering_alignment(offset: u64, data: &[u8]) -> bool {
    offset.is_multiple_of(SECTOR_ALIGN as u64)
        && (data.len() as u64).is_multiple_of(SECTOR_ALIGN as u64)
        && (data.as_ptr() as usize as u64).is_multiple_of(SECTOR_ALIGN as u64)
}

/// 底层对齐分配，由 Arc 共享以支持零拷贝 split
struct AlignedAlloc {
    /// 对齐分配的内存起始指针
    ptr: NonNull<u8>,
    /// 分配时的 Layout，Drop 时用相同 Layout dealloc
    layout: Layout,
}

// SAFETY: AlignedAlloc 不提供安全的写入接口。所有经 AlignedBuf 暴露的安全可变访问
// 都先以 Arc::get_mut 检查唯一性，必要时 COW 到独立分配后才写入。Arc 引用计数本身
// 绝不等同于可变访问的独占性；绕过该不变量的原始指针写入由调用方承担 unsafe 契约。
unsafe impl Send for AlignedAlloc {}
unsafe impl Sync for AlignedAlloc {}

impl AlignedAlloc {
    fn new(cap: usize, align: usize) -> io::Result<Self> {
        assert!(cap != 0, "AlignedAlloc capacity must be non-zero");
        let layout = Layout::from_size_align(cap, align)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid layout"))?;

        // SAFETY: layout.size() > 0，alloc 返回满足 layout.align 的对齐指针，或 null。
        let ptr = unsafe { alloc::alloc(layout) };
        let ptr = NonNull::new(ptr)
            .ok_or_else(|| io::Error::new(io::ErrorKind::OutOfMemory, "aligned alloc failed"))?;
        Ok(AlignedAlloc { ptr, layout })
    }
}

impl Drop for AlignedAlloc {
    fn drop(&mut self) {
        // SAFETY: AlignedAlloc 只接受非零容量；self.ptr 由 self.layout 对应的
        // alloc::alloc 分配，用相同非零 layout 调 dealloc 是 Rust 分配器契约的要求。
        unsafe { alloc::dealloc(self.ptr.as_ptr(), self.layout) };
    }
}

/// 从 `Arc<AlignedAlloc>` 中产出对齐 `Bytes` 的 owner 包装
struct AlignedBufOwner {
    alloc: Arc<AlignedAlloc>,
    offset: usize,
    len: usize,
}

// SAFETY: AlignedBufOwner 持有 Arc<AlignedAlloc>（Send + Sync），offset/len 是 usize。
// 它只经 AsRef 暴露初始化的只读字节；共享分配的安全写入必须先由 AlignedBuf COW 隔离。
unsafe impl Send for AlignedBufOwner {}

impl AsRef<[u8]> for AlignedBufOwner {
    fn as_ref(&self) -> &[u8] {
        if self.len == 0 {
            // SAFETY: AlignedAlloc 始终持有真实的非零分配；零容量视图仅是元数据，
            // 直接使用分配起始指针构造空切片，不进行 offset 指针运算。
            return unsafe { std::slice::from_raw_parts(self.alloc.ptr.as_ptr(), 0) };
        }

        // SAFETY: len > 0，offset + len <= alloc.layout.size()，且该范围由 AlignedBuf
        // 写入或 COW 复制初始化；因此 offset 位于实际分配内。
        unsafe { std::slice::from_raw_parts(self.alloc.ptr.as_ptr().add(self.offset), self.len) }
    }
}

/// 可指定对齐方式的写入缓冲区
///
/// `new` 分配 512 字节对齐内存；`with_align` 按请求的对齐值分配。支持零拷贝
/// `split()` 和 `freeze()`；`new` 创建的缓冲区，以及 `with_align` 请求兼容对齐值的
/// 缓冲区，`freeze()` 后的 `Bytes` 可满足 IOCP/WinFile 的 NO_BUFFERING 指针对齐要求。
pub struct AlignedBuf {
    alloc: Arc<AlignedAlloc>,
    /// 数据起始偏移（保持创建时请求的对齐）
    offset: usize,
    /// 已写入数据长度
    pos: usize,
    /// 从 offset 起的可用容量
    cap: usize,
}

impl std::fmt::Debug for AlignedBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlignedBuf")
            .field("ptr", &self.as_ptr())
            .field("offset", &self.offset)
            .field("pos", &self.pos)
            .field("cap", &self.cap)
            .finish()
    }
}

impl AlignedBuf {
    /// 分配指定容量的 512 对齐缓冲区
    ///
    /// 返回的 `AlignedBuf` 的 `as_ptr()` 满足 `SECTOR_ALIGN`（512）对齐。
    /// `cap` 必须为正数。
    pub fn new(cap: usize) -> io::Result<Self> {
        Self::with_align(cap, SECTOR_ALIGN)
    }

    /// 分配指定容量和对齐的缓冲区
    ///
    /// `align` 必须是 2 的幂。`cap` 必须为正数。
    #[allow(dead_code)] // 预留 API,未来可能支持非 512 对齐场景
    pub fn with_align(cap: usize, align: usize) -> io::Result<Self> {
        if cap == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cap must be non-zero",
            ));
        }
        let alloc = Arc::new(AlignedAlloc::new(cap, align)?);
        Ok(AlignedBuf {
            alloc,
            offset: 0,
            pos: 0,
            cap,
        })
    }

    /// 在任何安全写入前确保本缓冲独占底层分配。
    ///
    /// split/freeze 可以共享分配；此时仅复制已初始化的有效载荷，保留本缓冲的容量，
    /// 并将新分配的 offset 归零。零容量视图只是共享分配上的元数据，不能写入，
    /// 因而不脱离原分配。唯一所有者走零分配、零复制快路径。
    fn ensure_unique_for_write(&mut self) {
        if self.cap == 0 {
            return;
        }
        if Arc::get_mut(&mut self.alloc).is_some() {
            return;
        }

        let align = self.alloc.layout.align();
        // COW 分配失败(OOM)是不可恢复场景:无独立分配就无法安全写入(共享分配写入
        // 会破坏其他视图)。此 expect 触发条件为系统内存耗尽,属"不可恢复"错误;
        // panic=unwind 下该 panic 可被 catch_unwind 隔离,比 UB 可控。保留 expect,
        // 不为消除它而做大重构破坏 &mut self 无错误返回的 API 契约。
        let new_alloc =
            AlignedAlloc::new(self.cap, align).expect("AlignedBuf copy-on-write allocation failed");
        if self.pos != 0 {
            // SAFETY: self.pos <= self.cap，源 [offset, offset + pos) 是唯一已初始化
            // 区间；new_alloc 按 self.cap 分配且与源不重叠。此时只读取共享的旧分配。
            unsafe {
                std::ptr::copy_nonoverlapping(self.data_ptr(), new_alloc.ptr.as_ptr(), self.pos);
            }
        }

        self.alloc = Arc::new(new_alloc);
        self.offset = 0;
    }

    /// 返回当前窗口的原始起始指针，不创建引用。
    fn data_ptr(&self) -> *mut u8 {
        // soundness 不变量: offset + cap <= alloc_size。release 下也必须检查——
        // 旧实现用 debug_assert!(release 下编译消失),后续 unsafe { ptr.add(offset) }
        // 越界 → 堆越界 UB → 静默内存损坏或崩溃。panic=unwind 下 panic 可被
        // catch_unwind 隔离,UB 不可恢复,故宁 panic 不 UB。
        assert!(
            self.offset
                .checked_add(self.cap)
                .is_some_and(|end| end <= self.alloc.layout.size()),
            "AlignedBuf 不变量违反: offset({}) + cap({}) > alloc_size({})",
            self.offset,
            self.cap,
            self.alloc.layout.size()
        );
        if self.cap == 0 {
            return self.alloc.ptr.as_ptr();
        }
        // SAFETY: cap > 0 且 offset + cap <= alloc.layout.size()；故 offset 位于
        // AlignedAlloc 的实际分配内。这里只计算原始指针，不读取或写入字节。
        unsafe { self.alloc.ptr.as_ptr().add(self.offset) }
    }

    /// 数据起始指针，满足创建该缓冲区时请求的对齐值。
    ///
    /// `new` 创建的缓冲区满足 512 字节对齐；`with_align` 创建的缓冲区仅保证其传入的
    /// `align` 对齐。
    pub fn as_ptr(&self) -> *const u8 {
        self.data_ptr().cast_const()
    }

    /// 可变数据起始指针，满足创建该缓冲区时请求的对齐值。
    ///
    /// `new` 创建的缓冲区满足 512 字节对齐；`with_align` 创建的缓冲区仅保证其传入的
    /// `align` 对齐。返回前会执行 COW。
    ///
    /// # Safety
    ///
    /// 通过返回的原始指针写入时，调用方必须确保该分配始终由当前缓冲区独占；在
    /// `split`、`freeze` 或其他创建共享视图并触发 COW 的操作之后，不得再解引用旧指针。
    /// 原始指针访问不得与任何 `Bytes`、`as_slice()` 或 `as_mut_slice()` 暴露的视图重叠。
    /// 指针最多可访问 `capacity()` 个字节，且写入不会更新 `pos`；调用方必须避免使已初始化
    /// 范围的跟踪与实际写入不一致。
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ensure_unique_for_write();
        self.data_ptr()
    }

    /// 已写入数据的切片引用
    pub fn as_slice(&self) -> &[u8] {
        // soundness 不变量: [0, pos) 是唯一被 AlignedBuf 追踪为已初始化的范围,
        // 且 pos <= cap(由 as_mut_slice/extend_from_slice 的守卫与 split 的 cap=pos
        // 构造保证)。release 下也必须检查——旧实现完全无守卫,pos > cap 时
        // from_raw_parts 越界读 UB。panic=unwind 下可被 catch_unwind 隔离,UB 不可恢复。
        assert!(
            self.pos <= self.cap,
            "AlignedBuf 不变量违反: pos({}) > cap({})",
            self.pos,
            self.cap
        );
        // SAFETY: 上述 assert 保证 pos <= cap;[0, pos) 是唯一已初始化范围。
        unsafe { std::slice::from_raw_parts(self.as_ptr(), self.pos) }
    }

    /// 可写入区域的可变切片引用（覆盖整个 cap，非仅 pos 部分）。
    ///
    /// 此方法暴露完整容量；但没有提交逻辑长度的 API，因此写入 `len()` 之后的字节不会
    /// 纳入逻辑长度，也不会由 `freeze()` 产出。
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.ensure_unique_for_write();
        // soundness 不变量: pos <= cap。release 下也必须检查——旧实现用
        // debug_assert!(release 下编译消失),随后 `spare = cap - pos` 在 pos > cap
        // 时下溢为 usize::MAX,write_bytes 写入巨量字节 → 堆越界写 UB。
        assert!(
            self.pos <= self.cap,
            "AlignedBuf 不变量违反: pos({}) > cap({})",
            self.pos,
            self.cap
        );
        let ptr = self.data_ptr();
        let spare = self.cap - self.pos;

        // SAFETY: COW 后本缓冲独占该分配，ptr 覆盖 cap 个字节。AsMut 的全容量语义
        // 要求返回的每个 u8 均已初始化，因此先仅清零 [pos, cap) 的 spare 区，再构造
        // 切片；已写入的有效载荷不会被清零。
        unsafe {
            if spare != 0 {
                std::ptr::write_bytes(ptr.add(self.pos), 0, spare);
            }
            std::slice::from_raw_parts_mut(ptr, self.cap)
        }
    }

    /// 已写入数据长度
    pub fn len(&self) -> usize {
        self.pos
    }

    /// 是否未写入数据
    pub fn is_empty(&self) -> bool {
        self.pos == 0
    }

    /// 可用容量（从 offset 起）
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// 追加数据到缓冲区
    ///
    /// # Panics
    /// 如果剩余容量不足，panic。
    pub fn extend_from_slice(&mut self, data: &[u8]) {
        let new_pos = self
            .pos
            .checked_add(data.len())
            .expect("AlignedBuf position overflow");
        assert!(
            new_pos <= self.cap,
            "AlignedBuf 容量不足: pos={} + data={} > cap={}",
            self.pos,
            data.len(),
            self.cap
        );
        self.ensure_unique_for_write();
        // SAFETY: COW 后本缓冲独占分配；pos + data.len() <= cap 已断言，目标范围
        // 在有效分配内，且安全借用规则保证 data 不与目标范围重叠。
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), self.data_ptr().add(self.pos), data.len());
        }
        self.pos = new_pos;
    }

    /// 清空已写入数据（保留分配，cap 不变）
    ///
    /// 跨分片复用时调用，避免重新分配。
    pub fn clear(&mut self) {
        self.pos = 0;
    }

    /// 分离已写入的前缀，返回新的 `AlignedBuf`
    ///
    /// 零拷贝操作：新 `AlignedBuf` 共享底层 `AlignedAlloc`，offset 不变。
    /// 调用后 `self` 的 pos 归零，可继续写入。
    ///
    /// **对齐保持**：新缓冲区指针保持创建时请求的对齐。对于 `new` 创建的缓冲区，调用方在
    /// `WRITE_BATCH_BYTES`（256KB，512 的倍数）边界调用 split 时，`freeze()` 产出的
    /// `Bytes` 指针仍为 512 对齐。
    pub fn split(&mut self) -> AlignedBuf {
        let split_len = self.pos;
        let new_buf = AlignedBuf {
            alloc: Arc::clone(&self.alloc),
            offset: self.offset,
            pos: split_len,
            cap: split_len, // split 出的 buf cap = 已写入长度，不可继续写入
        };
        self.pos = 0; // 原 buf 归零，可继续写入（cap 不变）
        new_buf
    }

    /// 零拷贝转换为 `Bytes`
    ///
    /// 产出的 `Bytes` 的 `as_ptr()` 与 `self.as_ptr()` 相同，保持创建时请求的对齐；
    /// `new` 创建的缓冲区因此保持 512 字节对齐；`with_align` 则取决于调用方请求的对齐值。
    /// 通过 `Bytes::from_owner` 转移所有权，`Bytes` drop 时自动释放底层分配。
    pub fn freeze(self) -> Bytes {
        let owner = AlignedBufOwner {
            alloc: self.alloc,
            offset: self.offset,
            len: self.pos,
        };
        Bytes::from_owner(owner)
    }
}

impl AsRef<[u8]> for AlignedBuf {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

#[allow(dead_code)] // 预留 trait impl,未来可能被外部代码使用
impl AsMut<[u8]> for AlignedBuf {
    /// 返回覆盖完整容量的可变切片。
    ///
    /// 没有提交逻辑长度的 API，故对 `len()` 之后字节的写入不会改变逻辑长度，也不会由
    /// `freeze()` 产出。
    fn as_mut(&mut self) -> &mut [u8] {
        self.as_mut_slice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_allocates_aligned() {
        let buf = AlignedBuf::new(4096).unwrap();
        let ptr = buf.as_ptr() as usize;
        assert!(
            ptr.is_multiple_of(SECTOR_ALIGN),
            "指针 {ptr} 未按 {SECTOR_ALIGN} 对齐"
        );
    }

    #[test]
    fn test_new_rejects_zero_cap() {
        let err = AlignedBuf::new(0).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn test_extend_and_len() {
        let mut buf = AlignedBuf::new(256).unwrap();
        assert!(buf.is_empty());
        buf.extend_from_slice(b"hello");
        assert_eq!(buf.len(), 5);
        assert_eq!(buf.as_slice(), b"hello");
        buf.extend_from_slice(b" world");
        assert_eq!(buf.len(), 11);
        assert_eq!(buf.as_slice(), b"hello world");
    }

    #[test]
    #[should_panic(expected = "容量不足")]
    fn test_extend_panics_on_overflow() {
        let mut buf = AlignedBuf::new(4).unwrap();
        buf.extend_from_slice(b"hello");
    }

    #[test]
    fn test_clear() {
        let mut buf = AlignedBuf::new(256).unwrap();
        buf.extend_from_slice(b"data");
        assert!(!buf.is_empty());
        buf.clear();
        assert!(buf.is_empty());
        assert_eq!(buf.capacity(), 256);
    }

    #[test]
    fn test_split_preserves_alignment() {
        let mut buf = AlignedBuf::new(SECTOR_ALIGN * 4).unwrap();
        // 写入恰好 1 个 SECTOR_ALIGN 的数据
        buf.extend_from_slice(&vec![0xAB; SECTOR_ALIGN]);
        let split = buf.split();
        // split 后原 buf 归零
        assert!(buf.is_empty());
        // split 出的 buf 指针仍对齐
        let ptr = split.as_ptr() as usize;
        assert!(
            ptr.is_multiple_of(SECTOR_ALIGN),
            "split 后指针 {ptr} 未按 {SECTOR_ALIGN} 对齐"
        );
        assert_eq!(split.len(), SECTOR_ALIGN);
    }

    #[test]
    fn test_freeze_produces_aligned_bytes() {
        let mut buf = AlignedBuf::new(SECTOR_ALIGN * 2).unwrap();
        buf.extend_from_slice(&vec![0xCD; SECTOR_ALIGN]);
        let bytes = buf.freeze();
        let ptr = bytes.as_ptr() as usize;
        assert!(
            ptr.is_multiple_of(SECTOR_ALIGN),
            "freeze 后 Bytes 指针 {ptr} 未按 {SECTOR_ALIGN} 对齐"
        );
        assert_eq!(bytes.len(), SECTOR_ALIGN);
        assert_eq!(&bytes[..], &vec![0xCD; SECTOR_ALIGN]);
    }

    #[test]
    fn test_freeze_after_split_aligned() {
        // 模拟 downloader 的写入路径：extend 到 256KB -> split -> freeze
        let mut buf = AlignedBuf::new(WRITE_BATCH_BYTES).unwrap();
        buf.extend_from_slice(&vec![0xEF; WRITE_BATCH_BYTES]);
        let batch = buf.split().freeze();
        let ptr = batch.as_ptr() as usize;
        assert!(
            ptr.is_multiple_of(SECTOR_ALIGN),
            "split+freeze 后 Bytes 指针 {ptr} 未按 {SECTOR_ALIGN} 对齐"
        );
        assert_eq!(batch.len(), WRITE_BATCH_BYTES);
    }

    #[test]
    fn test_split_then_continue_writing() {
        // 模拟跨分片复用：split 后继续写入
        let mut buf = AlignedBuf::new(SECTOR_ALIGN * 4).unwrap();
        buf.extend_from_slice(&vec![0x11; SECTOR_ALIGN]);
        let _batch1 = buf.split();
        buf.extend_from_slice(&vec![0x22; SECTOR_ALIGN]);
        let batch2 = buf.split();
        assert_eq!(batch2.as_slice(), &vec![0x22; SECTOR_ALIGN]);
    }

    /// 回归：split().freeze() 后父缓冲继续写入不得污染已冻结视图。
    ///
    /// 审计动态证据：旧实现让父缓冲和冻结视图共享可写字节区，
    /// 父 extend 会覆写 freeze 仍持有的字节。正确设计保留父窗口，并在安全写入时隔离。
    #[test]
    fn test_split_freeze_not_aliased_with_parent_writes() {
        let mut parent = AlignedBuf::new(SECTOR_ALIGN * 4).unwrap();
        let secret = vec![0x53u8; SECTOR_ALIGN]; // 'S'
        parent.extend_from_slice(&secret);
        let frozen = parent.split().freeze();
        assert_eq!(&frozen[..], secret.as_slice());

        // 父缓冲继续写入：不得改写 frozen 内容
        let overwrite = vec![0x58u8; SECTOR_ALIGN]; // 'X'
        parent.extend_from_slice(&overwrite);
        assert_eq!(
            &frozen[..],
            secret.as_slice(),
            "split().freeze() 视图被父缓冲后续写入污染"
        );
        assert_eq!(parent.as_slice(), overwrite.as_slice());
    }

    /// 回归：空 split 子窗口的可变入口不得为零容量执行 COW 或伪造分配指针。
    #[test]
    fn test_empty_split_child_mut_slice_keeps_parent_allocation() {
        let mut parent = AlignedBuf::new(SECTOR_ALIGN * 2).unwrap();
        let original_parent_ptr = parent.as_ptr();
        let mut empty = parent.split();

        assert_eq!(empty.capacity(), 0);
        assert_eq!(empty.len(), 0);
        assert!(empty.as_mut_slice().is_empty());
        assert_eq!(
            empty.as_ptr(),
            original_parent_ptr,
            "零容量子窗口的可变入口不得脱离父缓冲的真实分配"
        );
        assert!((empty.as_ptr() as usize).is_multiple_of(SECTOR_ALIGN));
        assert!((parent.as_ptr() as usize).is_multiple_of(SECTOR_ALIGN));

        let frozen = empty.freeze();
        assert!(frozen.is_empty());

        let payload = b"parent remains usable";
        parent.extend_from_slice(payload);
        assert_eq!(parent.as_slice(), payload);
        assert!(frozen.is_empty(), "父缓冲写入不得改变已冻结的空结果");
    }

    /// 回归：`as_mut_ptr` 也必须经过 COW 门禁，不能暴露冻结快照的共享分配。
    #[test]
    fn test_as_mut_ptr_cow_detaches_from_frozen_snapshot() {
        let capacity = SECTOR_ALIGN * 4;
        let payload = vec![0xA7; SECTOR_ALIGN];
        let mut parent = AlignedBuf::new(capacity).unwrap();
        parent.extend_from_slice(&payload);
        let frozen = parent.split().freeze();
        let frozen_ptr = frozen.as_ptr();

        let parent_ptr = parent.as_mut_ptr();

        assert_ne!(parent_ptr.cast_const(), frozen_ptr);
        assert!((parent_ptr as usize).is_multiple_of(SECTOR_ALIGN));
        assert!((frozen_ptr as usize).is_multiple_of(SECTOR_ALIGN));
        assert_eq!(parent.capacity(), capacity);
        assert_eq!(&frozen[..], payload.as_slice());
    }

    /// 冻结视图释放后，父缓冲恢复独占并应在原分配上继续零拷贝复用。
    #[test]
    fn test_split_freeze_drop_reuses_original_parent_allocation() {
        let capacity = SECTOR_ALIGN * 4;
        let first_payload = vec![0x31; SECTOR_ALIGN];
        let second_payload = vec![0x42; SECTOR_ALIGN];
        let mut parent = AlignedBuf::new(capacity).unwrap();
        let original_ptr = parent.as_ptr();

        parent.extend_from_slice(&first_payload);
        let frozen = parent.split().freeze();
        assert_eq!(frozen.as_ptr(), original_ptr);
        assert_eq!(&frozen[..], first_payload.as_slice());
        drop(frozen);

        parent.extend_from_slice(&second_payload);

        assert_eq!(parent.as_ptr(), original_ptr);
        assert_eq!(parent.capacity(), capacity);
        assert_eq!(parent.as_slice(), second_payload.as_slice());
    }

    /// 安全的全容量 `&mut [u8]` API 不得暴露未初始化字节：spare 区必须先清零，
    /// 同时已经写入的有效前缀必须保持不变。
    #[test]
    fn test_as_mut_slice_initializes_full_capacity_and_preserves_prefix() {
        const CAPACITY: usize = 64;
        const NONZERO: u8 = 0xD5;
        let mut buf = AlignedBuf::new(CAPACITY).unwrap();

        buf.as_mut_slice().fill(NONZERO);
        buf.clear();
        assert!(buf.as_mut_slice().iter().all(|&byte| byte == 0));

        let prefix = b"initialized-prefix";
        buf.extend_from_slice(prefix);
        let full = buf.as_mut_slice();
        assert_eq!(&full[..prefix.len()], prefix);
        assert!(full[prefix.len()..].iter().all(|&byte| byte == 0));
    }

    /// 回归：非空 split 子缓冲通过安全可变切片修改后冻结，父缓冲复用不得污染快照。
    ///
    /// 仅修改子缓冲的已写入区间，不依赖父缓冲 split 后的 spare capacity 可写语义。
    #[test]
    fn test_mutated_nonempty_split_child_freeze_not_aliased_with_parent_writes() {
        let mut parent = AlignedBuf::new(SECTOR_ALIGN).unwrap();
        parent.extend_from_slice(b"original");
        let mut child = parent.split();
        assert_eq!(child.len(), 8);

        let child_len = child.len();
        child.as_mut_slice()[..child_len].copy_from_slice(b"snapshot");
        let frozen = child.freeze();
        assert_eq!(&frozen[..], b"snapshot");

        parent.extend_from_slice(b"XXXXXXXX");
        assert_eq!(
            &frozen[..],
            b"snapshot",
            "父缓冲复用污染了经 as_mut_slice 修改的 split 子缓冲快照"
        );
    }

    /// 任意非扇区长度也必须保持指针对齐和冻结快照隔离。
    #[test]
    fn test_non_sector_split_stays_aligned_and_isolated_from_parent_writes() {
        const NON_SECTOR_LEN: usize = 7;

        let mut parent = AlignedBuf::new(SECTOR_ALIGN).unwrap();
        let original = [0x31; NON_SECTOR_LEN];
        parent.extend_from_slice(&original);
        let frozen = parent.split().freeze();

        assert!((frozen.as_ptr() as usize).is_multiple_of(SECTOR_ALIGN));
        assert!((parent.as_ptr() as usize).is_multiple_of(SECTOR_ALIGN));
        assert_eq!(frozen.len(), NON_SECTOR_LEN);
        assert!(
            !satisfies_no_buffering_alignment(0, &frozen),
            "7 字节长度不应进入 NO_BUFFERING 对齐快速路径"
        );

        parent.extend_from_slice(&[0x32; NON_SECTOR_LEN]);
        assert_eq!(
            &frozen[..],
            original.as_slice(),
            "非扇区长度的冻结快照被父缓冲后续写入污染"
        );
    }

    /// split 后父缓冲应保留原容量和对齐窗口，以便批量缓冲复用。
    #[test]
    fn test_split_preserves_parent_capacity_and_aligned_window() {
        let original_capacity = SECTOR_ALIGN * 4;
        let mut parent = AlignedBuf::new(original_capacity).unwrap();
        let original_ptr = parent.as_ptr() as usize;
        parent.extend_from_slice(&vec![0xAA; SECTOR_ALIGN]);

        let split = parent.split();

        assert_eq!(split.len(), SECTOR_ALIGN);
        assert!((split.as_ptr() as usize).is_multiple_of(SECTOR_ALIGN));
        assert!(parent.is_empty());
        assert_eq!(parent.capacity(), original_capacity);
        assert_eq!(parent.as_ptr() as usize, original_ptr);
        assert!((parent.as_ptr() as usize).is_multiple_of(SECTOR_ALIGN));
    }

    #[test]
    fn test_large_buffer_alignment() {
        // 测试 WRITE_BATCH_BYTES（256KB）大小的分配对齐
        let buf = AlignedBuf::new(WRITE_BATCH_BYTES).unwrap();
        let ptr = buf.as_ptr() as usize;
        assert!(ptr.is_multiple_of(SECTOR_ALIGN));
        assert_eq!(buf.capacity(), WRITE_BATCH_BYTES);
    }

    /// AlignedBuf freeze 产出的 Bytes 满足 NO_BUFFERING 三向对齐契约:
    /// offset(0)、len(512 倍数)、ptr(512 对齐)均通过。
    /// 这是 AlignedBuf 与 IOCP/WinFile 对齐快速路径的端到端契约验证。
    #[test]
    fn test_aligned_buf_satisfies_no_buffering_alignment() {
        let mut buf = AlignedBuf::new(WRITE_BATCH_BYTES).unwrap();
        buf.extend_from_slice(&[0u8; WRITE_BATCH_BYTES]);
        let bytes = buf.freeze();
        assert!(satisfies_no_buffering_alignment(0, &bytes));
    }

    /// offset 非扇区对齐时返回 false(即使 buffer 指针对齐)。
    #[test]
    fn test_unaligned_offset_returns_false() {
        let mut buf = AlignedBuf::new(512).unwrap();
        buf.extend_from_slice(&[0u8; 512]);
        let bytes = buf.freeze();
        // offset=1 不是 512 的倍数
        assert!(!satisfies_no_buffering_alignment(1, &bytes));
    }

    /// 长度非扇区对齐时返回 false(即使 offset 和指针对齐)。
    #[test]
    fn test_unaligned_length_returns_false() {
        let mut buf = AlignedBuf::new(512).unwrap();
        buf.extend_from_slice(&[0u8; 100]); // 100 不是 512 的倍数
        let bytes = buf.freeze();
        assert!(!satisfies_no_buffering_alignment(0, &bytes));
    }

    /// AlignedBuf split 后产出的 Bytes 在 split 边界(512 倍数)仍满足对齐契约。
    /// 这验证了 downloader 的 WRITE_BATCH_BYTES(256KiB,512 的倍数)split 场景。
    #[test]
    fn test_split_preserves_no_buffering_alignment() {
        let mut buf = AlignedBuf::new(512 * 1024).unwrap();
        buf.extend_from_slice(&[0u8; 512 * 1024]);
        let split = buf.split();
        let bytes = split.freeze();
        assert!(satisfies_no_buffering_alignment(0, &bytes));
    }

    /// 回归: `data_ptr` 的 `offset + cap <= alloc_size` 不变量在 release 下也必须被检查。
    ///
    /// 旧实现用 `debug_assert!`,release 下编译消失(语言保证),任何边界 bug 会让
    /// `unsafe { ptr.add(offset) }` 越界 → 堆越界 UB → 静默内存损坏。升级为 `assert!`
    /// 后,release 下也 panic(panic=unwind 可被 catch_unwind 隔离,UB 不可恢复)。
    ///
    /// `data_ptr` 只计算指针不解引用,故旧实现(red)在 release 下不 panic、不触发 UB,
    /// `should_panic` 以"未 panic"失败;升级后(green)在 release 下也 panic。
    ///
    /// tests 作为子模块可访问父模块私有字段,此处手动破坏不变量以验证守卫触发。
    #[test]
    #[should_panic(expected = "AlignedBuf 不变量违反")]
    fn test_data_ptr_panics_on_offset_cap_overflow() {
        let mut buf = AlignedBuf::new(SECTOR_ALIGN).unwrap();
        // cap 设为超过分配大小,使 offset(0) + cap > alloc_size
        buf.cap = buf.alloc.layout.size() + 1;
        let _ = buf.data_ptr();
    }

    /// 回归: `as_slice` 的 `from_raw_parts(ptr, pos)` 依赖 `pos <= cap` 不变量。
    ///
    /// 旧实现**完全没有**该 assert(debug_assert 也没有),release 与 debug 下均无守卫,
    /// `pos > cap` 时 `from_raw_parts` 越界读 UB。新增 `assert!` 后,release 下也 panic。
    /// 这是本修复中新补的守卫(不只是 debug_assert→assert 升级)。
    #[test]
    #[should_panic(expected = "AlignedBuf 不变量违反")]
    fn test_as_slice_panics_on_pos_gt_cap() {
        let mut buf = AlignedBuf::new(SECTOR_ALIGN).unwrap();
        buf.pos = buf.cap + 1;
        let _ = buf.as_slice();
    }

    /// 回归: `as_mut_slice` 的 `pos <= cap` 不变量在 release 下也必须被检查。
    ///
    /// 旧实现用 `debug_assert!`,release 下编译消失,`spare = cap - pos` 减法下溢为
    /// `usize::MAX`,随后 `write_bytes(ptr.add(pos), 0, spare)` 写入巨量字节 → 堆越界
    /// 写 UB。升级为 `assert!` 后,在 `spare` 计算之前 panic,阻止 UB。
    #[test]
    #[should_panic(expected = "AlignedBuf 不变量违反")]
    fn test_as_mut_slice_panics_on_pos_gt_cap() {
        let mut buf = AlignedBuf::new(SECTOR_ALIGN).unwrap();
        buf.pos = buf.cap + 1;
        let _ = buf.as_mut_slice();
    }

    use tachyon_core::config::WRITE_BATCH_BYTES;
}
