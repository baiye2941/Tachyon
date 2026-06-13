//! 多镜像源 Protocol 适配器 (Happy Eyeballs v2 / RFC 8305)
//!
//! 包装主源和备用源列表,采用并行竞速策略:
//! - **probe**: 同时向所有源发起 HEAD 探测,选择最先响应的源
//! - **download**: 优先尝试主源(500ms 超时),失败后并行竞速所有镜像源
//!
//! 显著减少镜像切换时的等待时间,避免顺序 fallback 的串行延迟累积。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use tachyon_core::traits::Protocol;
use tachyon_core::types::FileMetadata;
use tachyon_core::{ByteStream, DownloadError, DownloadResult};

/// 多镜像源 Protocol 适配器
///
/// 包装主源和备用源列表,采用 Happy Eyeballs v2 (RFC 8305) 并行竞速策略:
/// - **probe**: 同时向所有源发起 HEAD 探测,选择最先响应的源
/// - **download**: 使用 probe 选中的源;若 probe 未执行,则优先尝试主源(500ms 超时),
///   失败后并行竞速所有镜像源
///
/// 显著减少镜像切换时的等待时间,避免顺序 fallback 的串行延迟累积。
pub(crate) struct MirrorProtocol {
    /// 主下载源
    primary: Arc<dyn Protocol>,
    /// 备用镜像源列表 (url, protocol)
    mirrors: Vec<(String, Arc<dyn Protocol>)>,
    /// probe 选中的源(由 probe 竞速设置,后续 download 方法优先使用)
    selected: Arc<Mutex<Option<Arc<dyn Protocol>>>>,
}

/// 主源快速尝试超时 (Happy Eyeballs 核心参数)
const PRIMARY_FAST_TIMEOUT: Duration = Duration::from_millis(500);

impl MirrorProtocol {
    pub(crate) fn new(
        primary: Arc<dyn Protocol>,
        mirrors: Vec<(String, Arc<dyn Protocol>)>,
    ) -> Self {
        Self {
            primary,
            mirrors,
            selected: Arc::new(Mutex::new(None)),
        }
    }

    /// 通用镜像源竞速核心逻辑
    ///
    /// 执行流程: selected 快径 -> 主源 500ms 快速尝试 -> 镜像并行竞速
    /// `download_fn` 抽象具体下载操作(范围/流式/全量),接收 Protocol 和 URL 返回异步结果
    async fn race_download<T: Send + 'static>(
        selected: &Arc<Mutex<Option<Arc<dyn Protocol>>>>,
        primary: Arc<dyn Protocol>,
        mirrors: &[(String, Arc<dyn Protocol>)],
        url: &str,
        download_fn: impl Fn(
            Arc<dyn Protocol>,
            String,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<T>> + Send>>
        + Clone
        + Send
        + 'static,
        error_label: &str,
    ) -> DownloadResult<T> {
        // 1. 优先使用 probe 选中的源
        if let Some(sel) = selected.lock().await.clone() {
            return download_fn(sel, url.to_string()).await;
        }

        // 2. 快速尝试主源(500ms 超时)
        match tokio::time::timeout(
            PRIMARY_FAST_TIMEOUT,
            download_fn(primary.clone(), url.to_string()),
        )
        .await
        {
            Ok(Ok(data)) => return Ok(data),
            Ok(Err(_)) | Err(_) => {
                tracing::info!(
                    "主源超时或失败,并行竞速 {} 个镜像{}",
                    mirrors.len(),
                    error_label
                );
            }
        }

        // 3. 并行竞速所有镜像源
        let mut set = JoinSet::new();
        for (mirror_url, proto) in mirrors {
            let p = proto.clone();
            let u = mirror_url.clone();
            let f = download_fn.clone();
            set.spawn(async move { f(p, u).await });
        }

        let mut first_err = None;
        while let Some(result) = set.join_next().await {
            match result {
                Ok(Ok(data)) => {
                    set.abort_all();
                    return Ok(data);
                }
                Ok(Err(e)) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(DownloadError::Io(std::io::Error::other(e.to_string())));
                    }
                }
            }
        }

        Err(first_err
            .unwrap_or_else(|| DownloadError::Protocol(format!("所有镜像源均失败{error_label}"))))
    }
}

impl Protocol for MirrorProtocol {
    fn probe(
        &self,
        url: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<FileMetadata>> + Send>>
    {
        let primary = self.primary.clone();
        let mirrors = self.mirrors.clone();
        let selected = self.selected.clone();
        let url = url.to_string();
        Box::pin(async move {
            if mirrors.is_empty() {
                let result = primary.probe(&url).await;
                if result.is_ok() {
                    *selected.lock().await = Some(primary);
                }
                return result;
            }

            // Happy Eyeballs: 并行竞速所有源的 probe
            // 用 (index, protocol) 标记每个源,以便获胜时记录选中项
            let mut set = JoinSet::new();
            set.spawn({
                let p = primary.clone();
                let u = url.clone();
                async move { (0usize, p.clone(), p.probe(&u).await) }
            });
            for (i, (mirror_url, proto)) in mirrors.iter().enumerate() {
                let p = proto.clone();
                let u = mirror_url.clone();
                set.spawn(async move { (i + 1, p.clone(), p.probe(&u).await) });
            }

            let mut last_err = None;
            while let Some(result) = set.join_next().await {
                match result {
                    Ok((_idx, proto, Ok(meta))) => {
                        set.abort_all();
                        *selected.lock().await = Some(proto);
                        return Ok(meta);
                    }
                    Ok((_idx, _proto, Err(e))) => last_err = Some(e),
                    Err(e) => {
                        last_err = Some(DownloadError::Io(std::io::Error::other(e.to_string())));
                    }
                }
            }
            Err(last_err.unwrap_or_else(|| DownloadError::Protocol("所有源探测均失败".into())))
        })
    }

    fn download_range(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>> {
        let primary = self.primary.clone();
        let mirrors = self.mirrors.clone();
        let selected = self.selected.clone();
        let url = url.to_string();
        Box::pin(async move {
            Self::race_download(
                &selected,
                primary,
                &mirrors,
                &url,
                move |proto, u| proto.download_range(&u, start, end),
                "",
            )
            .await
        })
    }

    fn download_range_stream(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>>
    {
        let primary = self.primary.clone();
        let mirrors = self.mirrors.clone();
        let selected = self.selected.clone();
        let url = url.to_string();
        Box::pin(async move {
            Self::race_download(
                &selected,
                primary,
                &mirrors,
                &url,
                move |proto, u| proto.download_range_stream(&u, start, end),
                "(流式)",
            )
            .await
        })
    }

    fn download_full(
        &self,
        url: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>> {
        let primary = self.primary.clone();
        let mirrors = self.mirrors.clone();
        let selected = self.selected.clone();
        let url = url.to_string();
        Box::pin(async move {
            Self::race_download(
                &selected,
                primary,
                &mirrors,
                &url,
                move |proto, u| proto.download_full(&u),
                "(全量)",
            )
            .await
        })
    }
}
