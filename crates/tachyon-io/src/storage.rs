//! 异步存储抽象

use std::future::Future;
use std::pin::Pin;

use bytes::{Bytes, BytesMut};

use tachyon_core::{DownloadError, DownloadResult};

pub trait AsyncStorage: Send + Sync {
    fn write_at(
        &self,
        offset: u64,
        data: Bytes,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>>;

    /// 写入 BytesMut 数据（避免 freeze() 产生额外复制）
    ///
    /// P1-05: 默认实现调用 `write_at(data.freeze())`，后端可覆盖以直接
    /// 使用 BytesMut 内部缓冲区，省去 freeze() 的原子引用计数操作。
    /// 对于 IOCP 等已预分配对齐缓冲区的后端，覆盖此方法可直接
    /// 从 BytesMut 的连续内存区拷贝到对齐缓冲区，无需中间 Bytes 转换。
    fn write_at_mut(
        &self,
        offset: u64,
        data: BytesMut,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
        Box::pin(async move { self.write_at(offset, data.freeze()).await })
    }

    fn read_at<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>>;

    fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;

    fn allocate(&self, size: u64) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;

    fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>>;

    fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;

    /// 对齐写入：自动处理 offset 和 data 的对齐填充。
    ///
    /// 4.1: 为 WinFile NO_BUFFERING 和 IoUringStorage O_DIRECT 等需要对齐的后端
    /// 提供统一的对齐写入 API。默认实现通过填充零字节将 offset 向下对齐、
    /// data 向上对齐到 `alignment` 边界，然后委托给 `write_at`。
    ///
    /// - `alignment` 必须为 2 的幂（典型值：512 扇区 / 4096 页）
    /// - 返回实际写入的用户数据字节数（等于 `data.len()`）
    fn write_at_aligned<'a>(
        &'a self,
        offset: u64,
        data: &'a [u8],
        alignment: u64,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
        Box::pin(async move {
            if alignment == 0 || !alignment.is_power_of_two() {
                return Err(DownloadError::Config(format!(
                    "alignment 必须为 2 的正整数幂, 实际值: {alignment}"
                )));
            }

            if data.is_empty() {
                return Ok(0);
            }

            let align_mask = alignment - 1;

            // 1. 将 offset 向下对齐到 alignment 边界
            let aligned_offset = offset & !align_mask;
            let front_pad = (offset - aligned_offset) as usize;

            // 2. 计算总填充大小（前端 + 数据 + 后端对齐）
            let total_len = front_pad + data.len();
            let padded_len = ((total_len as u64 + align_mask) & !align_mask) as usize;

            // 3. 构造对齐的写入缓冲区
            let mut padded = vec![0u8; padded_len];
            padded[front_pad..front_pad + data.len()].copy_from_slice(data);

            // 4. 委托给 write_at
            let written = self.write_at(aligned_offset, Bytes::from(padded)).await?;

            // 5. 返回用户数据的实际长度（而非填充后的长度）
            let user_written = written.saturating_sub(front_pad).min(data.len());
            Ok(user_written)
        })
    }
}
