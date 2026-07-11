//! 512 字节对齐的写入缓冲区
//!
//! IOCP（`FILE_FLAG_NO_BUFFERING`）和 WinFile（`FILE_FLAG_NO_BUFFERING`）要求
//! 写入数据的 buffer 指针按扇区大小（512 字节）对齐。标准 `BytesMut::with_capacity`
//! 底层 `Vec<u8>` 仅保证 1 字节对齐，导致 IOCP/WinFile 的对齐快速路径
//! （`submit_iocp_write` / 主句柄 `seek_write`）几乎永不触发，所有写入退化到
//! fallback 串行化路径。
//!
//! `AlignedBuf` 用 `std::alloc::alloc(Layout::from_size_align(cap, 512)?)` 分配
//! 512 对齐内存，通过 `Bytes::from_owner` 零拷贝产出指针对齐的 `Bytes`，
//! 使 IOCP/WinFile 对齐路径自动生效。
//!
//! ## 零拷贝 split 设计
//!
//! `BytesMut::split` 是零拷贝的（Arc 共享 + 偏移指针）。`AlignedBuf` 采用相同的
//! Arc 共享策略：`split()` 产出新 `AlignedBuf`（共享底层 `AlignedAlloc`），
//! 不复制数据。因 `WRITE_BATCH_BYTES=256KB` 是 512 的倍数，split 后的 offset
//! 仍为 512 对齐，`freeze()` 产出的 `Bytes` 指针也 512 对齐。

use std::alloc::{self, Layout};
use std::io;
use std::ptr::NonNull;
use std::sync::Arc;

use bytes::Bytes;

/// IOCP / WinFile NO_BUFFERING 的扇区对齐要求（字节）
pub const SECTOR_ALIGN: usize = 512;

/// 底层对齐分配，由 Arc 共享以支持零拷贝 split
struct AlignedAlloc {
    /// 对齐分配的内存起始指针
    ptr: NonNull<u8>,
    /// 分配时的 Layout，Drop 时用相同 Layout dealloc
    layout: Layout,
}

// SAFETY: AlignedAlloc 持有的内存通过 std::alloc::alloc 分配，指针在 Arc 生命周期内
// 有效。Arc 的引用计数保证同一时刻只有一个 owner 能修改数据区域（downloader 的
// worker 串行使用 write_buf），跨 worker 共享同一底层分配时各持有独立的 offset/pos，
// 不会重叠。内存本身无可变全局状态，Send + Sync 安全。
unsafe impl Send for AlignedAlloc {}
unsafe impl Sync for AlignedAlloc {}

impl AlignedAlloc {
    fn new(cap: usize, align: usize) -> io::Result<Self> {
        let layout = Layout::from_size_align(cap, align)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid layout"))?;
        // SAFETY: layout.size() > 0（调用方保证 cap > 0），alloc 返回满足 layout.align
        // 的对齐指针，或 null（分配失败）。
        let ptr = unsafe { alloc::alloc(layout) };
        let ptr = NonNull::new(ptr)
            .ok_or_else(|| io::Error::new(io::ErrorKind::OutOfMemory, "aligned alloc failed"))?;
        Ok(AlignedAlloc { ptr, layout })
    }
}

impl Drop for AlignedAlloc {
    fn drop(&mut self) {
        // SAFETY: self.ptr 由 self.layout 对应的 alloc::alloc 分配，
        // 用相同 layout 调 dealloc 是 Rust 分配器契约的要求。
        unsafe { alloc::dealloc(self.ptr.as_ptr(), self.layout) };
    }
}

/// 从 `Arc<AlignedAlloc>` 中产出对齐 `Bytes` 的 owner 包装
struct AlignedBufOwner {
    alloc: Arc<AlignedAlloc>,
    offset: usize,
    len: usize,
}

// SAFETY: AlignedBufOwner 持有 Arc<AlignedAlloc>（Send + Sync），offset/len 是 usize，
// 无其他可变状态。
unsafe impl Send for AlignedBufOwner {}

impl AsRef<[u8]> for AlignedBufOwner {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: alloc.ptr 指向 AlignedAlloc 分配的内存，offset + len <= alloc.cap
        // （由 AlignedBuf 的不变量保证），切片在有效区间内。
        unsafe { std::slice::from_raw_parts(self.alloc.ptr.as_ptr().add(self.offset), self.len) }
    }
}

/// 512 字节对齐的写入缓冲区
///
/// 用 `std::alloc` 分配对齐内存，支持零拷贝 `split()` 和 `freeze()`。
/// `freeze()` 产出的 `Bytes` 的 `as_ptr()` 满足 512 对齐，使 IOCP/WinFile
/// 的 NO_BUFFERING 对齐快速路径生效。
pub struct AlignedBuf {
    alloc: Arc<AlignedAlloc>,
    /// 数据起始偏移（始终是 SECTOR_ALIGN 的倍数）
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

    /// 数据起始指针（512 对齐）
    pub fn as_ptr(&self) -> *const u8 {
        // SAFETY: alloc.ptr 有效，offset <= alloc.cap（不变量）
        unsafe { self.alloc.ptr.as_ptr().add(self.offset) }
    }

    /// 可变数据起始指针（512 对齐）
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        // SAFETY: alloc.ptr 有效，offset <= alloc.cap（不变量）。&mut self 保证独占。
        unsafe { self.alloc.ptr.as_ptr().add(self.offset) }
    }

    /// 已写入数据的切片引用
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: as_ptr 有效，pos <= cap（不变量）
        unsafe { std::slice::from_raw_parts(self.as_ptr(), self.pos) }
    }

    /// 可写入区域的可变切片引用（覆盖整个 cap，非仅 pos 部分）
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as_mut_ptr 有效，cap <= alloc.cap - offset（不变量）
        unsafe { std::slice::from_raw_parts_mut(self.as_mut_ptr(), self.cap) }
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
        let new_pos = self.pos + data.len();
        assert!(
            new_pos <= self.cap,
            "AlignedBuf 容量不足: pos={} + data={} > cap={}",
            self.pos,
            data.len(),
            self.cap
        );
        // SAFETY: as_mut_ptr 有效，pos + data.len() <= cap（已 assert）
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                self.as_mut_ptr().add(self.pos),
                data.len(),
            );
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
    /// 零拷贝操作：新 `AlignedBuf` 共享底层 `AlignedAlloc`，offset 不变（仍 512 对齐）。
    /// 调用后 `self` 的 pos 归零，可继续写入。
    ///
    /// **对齐保持**：因调用方在 `WRITE_BATCH_BYTES`（256KB，512 的倍数）边界调用 split，
    /// 新 `AlignedBuf` 的 offset 保持 512 对齐，`freeze()` 产出的 `Bytes` 指针也 512 对齐。
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
    /// 产出的 `Bytes` 的 `as_ptr()` 与 `self.as_ptr()` 相同，满足 512 对齐。
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

impl AsMut<[u8]> for AlignedBuf {
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
            ptr % SECTOR_ALIGN == 0,
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
            ptr % SECTOR_ALIGN == 0,
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
            ptr % SECTOR_ALIGN == 0,
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
            ptr % SECTOR_ALIGN == 0,
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

    #[test]
    fn test_large_buffer_alignment() {
        // 测试 WRITE_BATCH_BYTES（256KB）大小的分配对齐
        let buf = AlignedBuf::new(WRITE_BATCH_BYTES).unwrap();
        let ptr = buf.as_ptr() as usize;
        assert!(ptr % SECTOR_ALIGN == 0);
        assert_eq!(buf.capacity(), WRITE_BATCH_BYTES);
    }

    use tachyon_core::config::WRITE_BATCH_BYTES;
}
