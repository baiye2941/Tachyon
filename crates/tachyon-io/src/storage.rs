//! 异步存储抽象

use bytes::Bytes;

use tachyon_core::DownloadResult;

pub trait AsyncStorage: Send + Sync {
    fn write_at(
        &self,
        offset: u64,
        data: Bytes,
    ) -> impl std::future::Future<Output = DownloadResult<usize>> + Send;

    fn read_at(
        &self,
        offset: u64,
        buf: &mut [u8],
    ) -> impl std::future::Future<Output = DownloadResult<usize>> + Send;

    fn sync(&self) -> impl std::future::Future<Output = DownloadResult<()>> + Send;

    fn allocate(&self, size: u64) -> impl std::future::Future<Output = DownloadResult<()>> + Send;

    fn file_size(&self) -> impl std::future::Future<Output = DownloadResult<u64>> + Send;

    fn close(&self) -> impl std::future::Future<Output = DownloadResult<()>> + Send;
}
