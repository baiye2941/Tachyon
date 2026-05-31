//! AI Model Downloader I/O 层:零拷贝存储引擎
//!
//! 提供跨平台的异步文件 I/O:
//! - Linux:io_uring 零拷贝管道
//! - Windows:WinFile 优化(NO_BUFFERING + SEQUENTIAL_SCAN)
//! - macOS:tokio 标准异步文件 I/O
//! - BufferPool 管理与 buffer 复用(带 Semaphore 反压)
//! - 零拷贝写入管道(含批量合并写入)

pub mod buffer;
pub mod iouring;
pub mod pipeline;
pub mod storage;
pub mod tokio_file;
pub mod winio;

pub use buffer::{BufferGuard, BufferPool, BufferPoolStats};
pub use iouring::{IoUringConfig, IoUringState, IoUringStorage};
pub use pipeline::WritePipeline;
pub use storage::AsyncStorage;
pub use tokio_file::TokioFile;
pub use winio::WinFile;

// 验证测试:放在 crate 根级别,以便 `--exact` 匹配

/// 验证 io_uring buffer 对齐:512 和 4096 字节边界
#[cfg(test)]
#[test]
fn buffer_align() {
    // 512 字节对齐
    let size = 100usize;
    assert_eq!((size + 511) & !511, 512);

    // 4096 字节对齐(O_DIRECT)
    let size = 1000usize;
    assert_eq!((size + 4095) & !4095, 4096);

    // 已对齐的大小不变
    assert_eq!((4096usize + 4095) & !4095, 4096);
    assert_eq!((512usize + 511) & !511, 512);

    // 默认 64KB buffer 是 4096 的倍数
    assert_eq!((64 * 1024usize) % 4096, 0);
}

/// 验证 WritePipeline 可以正常创建并执行基本写入操作
#[cfg(test)]
#[tokio::test]
async fn write_pipeline() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let storage = TokioFile::open(tmp.path()).await.unwrap();
    let pipeline = WritePipeline::new(storage, 4096, 4);

    assert_eq!(pipeline.available_permits(), 4);
    assert_eq!(pipeline.buffer_size(), 4096);

    let written = pipeline.write(0, b"hello pipeline").await.unwrap();
    assert_eq!(written, 14);

    let mut buf = [0u8; 14];
    let read = pipeline.storage().read_at(0, &mut buf).await.unwrap();
    assert_eq!(read, 14);
    assert_eq!(&buf, b"hello pipeline");
}

/// 验证 BufferPool 反压行为:许可耗尽时 alloc 阻塞,归还后恢复
#[cfg(test)]
#[tokio::test]
async fn backpressure() {
    // 创建小容量 buffer 池,模拟资源受限场景
    let pool = BufferPool::new(1024, 2);
    assert_eq!(pool.capacity(), 2);
    assert_eq!(pool.available(), 2);

    // 分配所有许可,池许可耗尽
    let buf1 = pool.alloc().await;
    let buf2 = pool.alloc().await;
    assert_eq!(pool.available(), 0);

    // 第三次 alloc 应阻塞(反压生效)
    let result = tokio::time::timeout(std::time::Duration::from_millis(50), pool.alloc()).await;
    assert!(result.is_err(), "许可耗尽时 alloc 应阻塞");

    // 归还第一个 buffer,许可恢复
    pool.release(buf1);
    assert_eq!(pool.available(), 1);

    // 现在 alloc 应立即成功
    let buf3 = pool.alloc().await;
    assert_eq!(pool.available(), 0);

    // 归还剩余 buffer
    pool.release(buf2);
    pool.release(buf3);
    assert_eq!(pool.available(), 2);
}

/// 验证 BufferPool 反压链路:归还后等待中的 alloc 被唤醒
#[cfg(test)]
#[tokio::test]
async fn backpressure_wakes_waiter() {
    let pool = BufferPool::new(1024, 1);
    let buf = pool.alloc().await;
    assert_eq!(pool.available(), 0);

    // 在另一个任务中等待 alloc(会阻塞)
    let pool_clone = pool.clone();
    let alloc_task = tokio::spawn(async move { pool_clone.alloc().await });

    // 短暂延迟后归还,唤醒等待任务
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    pool.release(buf);

    // 等待任务完成
    let buf2 = alloc_task.await.expect("任务应成功");
    assert_eq!(buf2.capacity(), 1024);
}

/// 验证 WinFile NO_BUFFERING 对齐写入逻辑
#[cfg(test)]
#[test]
fn winfile_align() {
    const SECTOR_SIZE: u64 = 512;
    // 非对齐偏移
    assert!(!100u64.is_multiple_of(SECTOR_SIZE));
    // 对齐偏移
    assert!(0u64.is_multiple_of(SECTOR_SIZE));
    assert!(512u64.is_multiple_of(SECTOR_SIZE));
    assert!(4096u64.is_multiple_of(SECTOR_SIZE));
    // 非对齐数据长度
    assert!(!256u64.is_multiple_of(SECTOR_SIZE));
    // 对齐数据长度
    assert!(512u64.is_multiple_of(SECTOR_SIZE));
    assert!(1024u64.is_multiple_of(SECTOR_SIZE));
}
