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

    /// 清除已选中的源,使下次下载重新竞速所有镜像
    pub(crate) async fn clear_selected(&self) {
        *self.selected.lock().await = None;
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
    fn clear_selected(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move { self.clear_selected().await })
    }

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

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::time::Duration;

    use bytes::Bytes;
    use futures::StreamExt;

    use super::MirrorProtocol;
    use tachyon_core::traits::Protocol;
    use tachyon_core::types::FileMetadata;
    use tachyon_core::{ByteStream, DownloadError, DownloadResult};

    #[derive(Clone)]
    struct MockProtocol {
        probe_delay: Duration,
        probe_meta: Result<FileMetadata, String>,
        download_data: Result<Bytes, String>,
    }

    impl MockProtocol {
        fn new() -> Self {
            Self {
                probe_delay: Duration::ZERO,
                probe_meta: Ok(FileMetadata {
                    file_name: "mock".into(),
                    file_size: Some(100),
                    content_type: None,
                    supports_range: true,
                    etag: None,
                    last_modified: None,
                }),
                download_data: Ok(Bytes::from_static(b"mock")),
            }
        }

        fn with_probe_delay(mut self, delay: Duration) -> Self {
            self.probe_delay = delay;
            self
        }

        fn with_probe_meta(mut self, meta: Result<FileMetadata, String>) -> Self {
            self.probe_meta = meta;
            self
        }

        fn with_download_data(mut self, data: Result<Bytes, String>) -> Self {
            self.download_data = data;
            self
        }
    }

    impl Protocol for MockProtocol {
        fn probe(
            &self,
            _url: &str,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<FileMetadata>> + Send>> {
            let delay = self.probe_delay;
            let result = self.probe_meta.clone();
            Box::pin(async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                result.map_err(DownloadError::Protocol)
            })
        }

        fn download_range(
            &self,
            _url: &str,
            _start: u64,
            _end: u64,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
            let result = self.download_data.clone();
            Box::pin(async move { result.map_err(DownloadError::Protocol) })
        }

        fn download_range_stream(
            &self,
            _url: &str,
            _start: u64,
            _end: u64,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
            let result = self.download_data.clone();
            Box::pin(async move {
                let data = result.map_err(DownloadError::Protocol)?;
                let stream = futures::stream::once(async move { Ok(data) });
                Ok(Box::pin(stream) as ByteStream)
            })
        }

        fn download_full(
            &self,
            _url: &str,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
            let result = self.download_data.clone();
            Box::pin(async move { result.map_err(DownloadError::Protocol) })
        }
    }

    #[tokio::test]
    async fn test_probe_selects_fastest_mirror() {
        tokio::time::pause();

        let slow_primary = Arc::new(
            MockProtocol::new()
                .with_probe_delay(Duration::from_secs(10))
                .with_probe_meta(Ok(FileMetadata {
                    file_name: "primary".into(),
                    file_size: Some(100),
                    content_type: None,
                    supports_range: true,
                    etag: None,
                    last_modified: None,
                })),
        );

        let fast_mirror = Arc::new(
            MockProtocol::new()
                .with_probe_delay(Duration::from_millis(100))
                .with_probe_meta(Ok(FileMetadata {
                    file_name: "mirror".into(),
                    file_size: Some(200),
                    content_type: None,
                    supports_range: true,
                    etag: None,
                    last_modified: None,
                })),
        );

        let mirror_protocol = MirrorProtocol::new(
            slow_primary,
            vec![("http://mirror.com/file".into(), fast_mirror)],
        );

        let result = mirror_protocol.probe("http://primary.com/file").await;
        assert!(result.is_ok(), "probe 应返回最快镜像的结果");
        assert_eq!(
            result.unwrap().file_size,
            Some(200),
            "应选择最快镜像(file_size=200)"
        );
    }

    #[tokio::test]
    async fn test_clear_selected_resets_probe_choice() {
        let primary = Arc::new(MockProtocol::new().with_probe_meta(Ok(FileMetadata {
            file_name: "primary".into(),
            file_size: Some(100),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
        })));

        let mirror = Arc::new(MockProtocol::new().with_probe_meta(Ok(FileMetadata {
            file_name: "mirror".into(),
            file_size: Some(200),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
        })));

        let mirror_protocol =
            MirrorProtocol::new(primary, vec![("http://mirror.com/file".into(), mirror)]);

        let meta = mirror_protocol
            .probe("http://primary.com/file")
            .await
            .unwrap();
        assert_eq!(meta.file_size, Some(100), "竞速应选中主源");
        assert!(
            mirror_protocol.selected.lock().await.is_some(),
            "probe 后应记录已选源"
        );

        mirror_protocol.clear_selected().await;
        assert!(
            mirror_protocol.selected.lock().await.is_none(),
            "clear_selected 后应清空已选源"
        );
    }

    #[tokio::test]
    async fn test_download_fallback_to_mirror_when_primary_fails() {
        let failing_primary =
            Arc::new(MockProtocol::new().with_download_data(Err("primary failed".into())));

        let working_mirror = Arc::new(
            MockProtocol::new().with_download_data(Ok(Bytes::from_static(b"mirror data"))),
        );

        let mirror_protocol = MirrorProtocol::new(
            failing_primary,
            vec![("http://mirror.com/file".into(), working_mirror)],
        );

        let result = mirror_protocol
            .download_full("http://primary.com/file")
            .await;
        assert_eq!(
            result.unwrap(),
            Bytes::from_static(b"mirror data"),
            "主源失败时应回退到镜像源"
        );
    }

    #[tokio::test]
    async fn test_download_range_fallback_to_mirror_when_primary_fails() {
        let failing_primary =
            Arc::new(MockProtocol::new().with_download_data(Err("primary range failed".into())));

        let working_mirror = Arc::new(
            MockProtocol::new().with_download_data(Ok(Bytes::from_static(b"mirror range"))),
        );

        let mirror_protocol = MirrorProtocol::new(
            failing_primary,
            vec![("http://mirror.com/file".into(), working_mirror)],
        );

        let result = mirror_protocol
            .download_range("http://primary.com/file", 0, 99)
            .await;
        assert_eq!(
            result.unwrap(),
            Bytes::from_static(b"mirror range"),
            "主源 range 失败时应回退到镜像源"
        );
    }

    #[tokio::test]
    async fn test_download_range_stream_fallback_to_mirror_when_primary_fails() {
        let failing_primary =
            Arc::new(MockProtocol::new().with_download_data(Err("primary stream failed".into())));

        let working_mirror = Arc::new(
            MockProtocol::new().with_download_data(Ok(Bytes::from_static(b"mirror stream"))),
        );

        let mirror_protocol = MirrorProtocol::new(
            failing_primary,
            vec![("http://mirror.com/file".into(), working_mirror)],
        );

        let result = mirror_protocol
            .download_range_stream("http://primary.com/file", 0, 99)
            .await;
        assert!(result.is_ok(), "主源流式失败时应回退到镜像源");

        let mut stream = result.unwrap();
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(chunk, Bytes::from_static(b"mirror stream"));
    }
}
