//! 核心 trait 定义
//!
//! 所有 crate 共享的公共接口抽象

use std::future::Future;
use std::pin::Pin;

use bytes::Bytes;
use futures::Stream;

use crate::config::DownloadConfig;
use crate::error::DownloadResult;
use crate::types::{FileMetadata, FragmentInfo, TaskId};

/// 字节流类型别名
///
/// 用于 `download_range_stream` 的返回值,逐块产出 `DownloadResult<Bytes>`。
/// 调用方应使用 `StreamExt::next()` 逐块消费,避免将整个响应缓冲到内存。
pub type ByteStream = Pin<Box<dyn Stream<Item = DownloadResult<Bytes>> + Send>>;

/// 协议层 trait:负责与远程服务器通信
///
/// 使用 `Pin<Box<dyn Future>>` 返回类型以满足 object-safe 条件,
/// 支持 `Arc<dyn Protocol>` 动态分发。
///
/// 返回的 Future 生命周期为 `'static`,因为 `Arc<dyn Protocol>` 持有协议实例的所有权,
/// 调用方在 await 期间自行保证 self 和 url 的借用有效性。
pub trait Protocol: Send + Sync {
    /// 探测远程文件元数据(大小、是否支持 Range 等)
    fn probe(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<FileMetadata>> + Send>>;

    /// 下载指定字节范围的数据
    fn download_range(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>>;

    /// 流式下载指定字节范围的数据
    ///
    /// 与 `download_range` 不同,此方法以流式方式返回数据块,
    /// 允许调用方边接收边写入存储,降低峰值内存占用。
    /// 调用方应使用 `StreamExt::next()` 逐块消费。
    fn download_range_stream(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>>;

    /// 下载整个文件(不支持 Range 时使用)
    fn download_full(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>>;

    /// 流式下载整个文件(不支持 Range 时使用)
    ///
    /// 与 `download_full` 不同,此方法以流式方式返回数据块,调用方边接收边写入,
    /// 峰值内存仅含单个 chunk,避免大文件整块进内存。
    ///
    /// 默认实现回退到 `download_full` 并包装为单块流,保证所有实现者无需改动即可工作;
    /// HTTP 等支持流式的协议应覆盖此方法以获得真正的低内存流式下载。
    fn download_full_stream(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
        let fut = self.download_full(url);
        Box::pin(async move {
            let data = fut.await?;
            Ok(Box::pin(futures::stream::once(async move { Ok(data) })) as ByteStream)
        })
    }
}

pub trait Storage: Send + Sync {
    fn write_at(
        &self,
        offset: u64,
        data: Bytes,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>>;

    fn read_at<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>>;

    fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;

    fn allocate(&self, size: u64) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;

    fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>>;

    fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;
}

/// 校验层 trait:负责数据完整性校验
pub trait Verifier: Send + Sync {
    /// 计算数据的哈希值
    fn compute_hash(&self, data: &[u8]) -> DownloadResult<String>;

    /// 校验数据是否匹配预期哈希
    fn verify(&self, data: &[u8], expected_hash: &str) -> DownloadResult<()> {
        let actual = self.compute_hash(data)?;
        if actual == expected_hash {
            Ok(())
        } else {
            Err(crate::error::DownloadError::ChecksumMismatch {
                expected: expected_hash.to_string(),
                actual,
            })
        }
    }
}

/// 下载任务 trait:表示一个完整的下载任务
pub trait DownloadTask: Send + Sync {
    /// 获取任务 ID
    fn id(&self) -> TaskId;

    /// 获取下载 URL
    fn url(&self) -> &str;

    /// 获取下载配置
    fn config(&self) -> &DownloadConfig;

    /// 获取文件元数据
    fn metadata(&self) -> DownloadResult<&FileMetadata>;

    /// 获取所有分片信息
    fn fragments(&self) -> &[FragmentInfo];

    /// 获取当前状态
    fn state(&self) -> crate::types::DownloadState;

    /// 计算总体下载进度(0.0 ~ 1.0)
    fn progress(&self) -> f64 {
        let fragments = self.fragments();
        if fragments.is_empty() {
            return 0.0;
        }
        let total: u64 = fragments.iter().map(|f| f.size).sum();
        if total == 0 {
            return 1.0;
        }
        let downloaded: u64 = fragments.iter().map(|f| f.downloaded).sum();
        downloaded as f64 / total as f64
    }
}

/// 分片下载 trait:单个分片的下载操作
pub trait FragmentDownloader: Send + Sync {
    /// 下载单个分片
    fn download(
        &self,
        task_id: TaskId,
        fragment: &FragmentInfo,
    ) -> impl std::future::Future<Output = DownloadResult<Bytes>> + Send;

    /// 取消分片下载
    fn cancel(&self, task_id: TaskId, fragment_index: u32) -> DownloadResult<()>;
}

/// 下载调度建议
///
/// 调度器根据带宽预测和文件特征返回的动态配置建议。
#[derive(Debug, Clone)]
pub struct ScheduleRecommendation {
    /// 建议的并发分片数
    pub concurrency: u32,
    /// 建议的分片大小(字节)
    pub fragment_size: u64,
    /// 带宽预测置信度(0.0 ~ 1.0)
    pub confidence: f64,
}

impl Default for ScheduleRecommendation {
    fn default() -> Self {
        Self {
            concurrency: 4,
            fragment_size: 4 * 1024 * 1024, // 4MB
            confidence: 0.0,
        }
    }
}

/// 下载调度器 trait:提供智能调度决策
///
/// 调度器负责:
/// - 基于带宽预测推荐并发度
/// - 根据网络状况动态调整分片大小
/// - 提供调度建议的置信度评估
pub trait DownloadScheduler: Send + Sync {
    /// 记录带宽观测值
    fn observe_bandwidth(&self, bytes_per_sec: u64);

    /// 获取调度建议
    ///
    /// 根据当前带宽预测、文件大小和配置约束,返回最优的并发度和分片大小建议。
    fn recommend(&self, file_size: u64, max_concurrency: u32) -> ScheduleRecommendation;

    /// 获取当前带宽预测(字节/秒)
    fn predicted_bandwidth(&self) -> u64;
}
