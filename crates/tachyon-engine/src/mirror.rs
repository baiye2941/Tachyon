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
/// probe 选中的源(协议 + 对应 URL)
type SelectedSource = (Arc<dyn Protocol>, String);

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
    ///
    /// 同时记录选中源对应的 URL。probe 选中镜像后,后续 download 必须用该镜像 URL
    /// 而非主源 URL,否则镜像协议会拿着主源 URL 请求镜像服务器导致失败。
    selected: Arc<Mutex<Option<SelectedSource>>>,
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
        selected: &Arc<Mutex<Option<SelectedSource>>>,
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
        // 1. 优先使用 probe 选中的源及其对应 URL;失败则清空选中,落到全源竞速
        //
        // 不能在 selected 命中时直接 return:probe 阶段某源可能"伪成功"(如 DNS 污染下
        // 官方源返回 200/302),被选中后实际下载却失败。若不回退,镜像永远不被尝试,
        // 表现为"竞速模式仍只走主源"。故 selected 仅作优先提示,失败必须回退竞速。
        //
        // 注意:必须显式 drop guard 后再清空,否则若在持锁期间再次 lock 会死锁
        // (tokio::sync::Mutex 不可重入)。
        let selected_clone = selected.lock().await.clone();
        if let Some((sel, sel_url)) = selected_clone {
            match download_fn(sel, sel_url).await {
                Ok(data) => return Ok(data),
                Err(e) => {
                    tracing::info!(error = %e, "probe 选中源下载失败,回退全源竞速");
                    *selected.lock().await = None;
                }
            }
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
                    *selected.lock().await = Some((primary, url.clone()));
                }
                return result;
            }

            // Happy Eyeballs: 并行竞速所有源的 probe
            // 用 (index, protocol, url) 标记每个源,以便获胜时记录选中项及对应 URL
            let mut set = JoinSet::new();
            set.spawn({
                let p = primary.clone();
                let u = url.clone();
                async move { (0usize, p.clone(), u.clone(), p.probe(&u).await) }
            });
            for (i, (mirror_url, proto)) in mirrors.iter().enumerate() {
                let p = proto.clone();
                let u = mirror_url.clone();
                set.spawn(async move { (i + 1, p.clone(), u.clone(), p.probe(&u).await) });
            }

            let mut last_err = None;
            while let Some(result) = set.join_next().await {
                match result {
                    Ok((_idx, proto, sel_url, Ok(meta))) => {
                        set.abort_all();
                        *selected.lock().await = Some((proto, sel_url));
                        return Ok(meta);
                    }
                    Ok((_idx, _proto, _url, Err(e))) => last_err = Some(e),
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
        /// 若设置,download_* 收到不匹配此 URL 的请求则失败(用于验证竞速选中后用对 URL)
        expected_url: Option<String>,
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
                expected_url: None,
            }
        }

        fn with_probe_delay(mut self, delay: Duration) -> Self {
            self.probe_delay = delay;
            self
        }

        fn with_expected_url(mut self, url: impl Into<String>) -> Self {
            self.expected_url = Some(url.into());
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
            url: &str,
            _start: u64,
            _end: u64,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
            let result = self.download_data.clone();
            let expected = self.expected_url.clone();
            let url = url.to_string();
            Box::pin(async move {
                if let Some(exp) = expected
                    && exp != url
                {
                    return Err(DownloadError::Protocol(format!(
                        "URL 不匹配: 期望 {exp}, 实际 {url}"
                    )));
                }
                result.map_err(DownloadError::Protocol)
            })
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
            url: &str,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
            let result = self.download_data.clone();
            let expected = self.expected_url.clone();
            let url = url.to_string();
            Box::pin(async move {
                if let Some(exp) = expected
                    && exp != url
                {
                    return Err(DownloadError::Protocol(format!(
                        "URL 不匹配: 期望 {exp}, 实际 {url}"
                    )));
                }
                result.map_err(DownloadError::Protocol)
            })
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

    /// 验证 probe 选中镜像后,后续 download 用选中源对应的 URL 而非主源 URL。
    ///
    /// 回归测试:曾因 selected 只存协议不存 URL,导致 probe 选中镜像后 download_range
    /// 仍传主源 URL,镜像协议拿着主源 URL 请求镜像服务器而失败(HF Race 模式失效)。
    #[tokio::test]
    async fn test_download_uses_selected_mirror_url_not_primary_url() {
        // 主源 probe 失败(模拟官方源国内探测不通),下载也失败
        let primary = Arc::new(
            MockProtocol::new()
                .with_probe_meta(Err("primary probe failed".into()))
                .with_download_data(Err("primary download failed".into())),
        );

        // 镜像 probe 成功,且只接受镜像 URL(expected_url),收到主源 URL 则失败
        let mirror = Arc::new(
            MockProtocol::new()
                .with_probe_meta(Ok(FileMetadata {
                    file_name: "model.bin".into(),
                    file_size: Some(100),
                    content_type: None,
                    supports_range: true,
                    etag: None,
                    last_modified: None,
                }))
                .with_download_data(Ok(Bytes::from_static(b"from mirror")))
                .with_expected_url("http://mirror.com/file"),
        );

        let mirror_protocol =
            MirrorProtocol::new(primary, vec![("http://mirror.com/file".into(), mirror)]);

        // probe 竞速:仅镜像成功,选中镜像并记录镜像 URL
        let meta = mirror_protocol.probe("http://primary.com/file").await;
        assert!(meta.is_ok(), "probe 应选中镜像成功");

        // 下载:必须用镜像 URL 才能成功。若错误地用主源 URL,镜像会因 expected_url 不匹配而失败
        let result = mirror_protocol
            .download_range("http://primary.com/file", 0, 99)
            .await;
        assert!(
            result.is_ok(),
            "probe 选中镜像后下载应使用镜像 URL,实际错误: {:?}",
            result.err()
        );
        assert_eq!(result.unwrap(), Bytes::from_static(b"from mirror"));
    }

    /// 验证 probe 选中源下载失败时,回退到全源竞速(而非锁死在选中源)。
    ///
    /// 回归测试:Race 模式下 probe 阶段官方源可能"伪成功"(DNS 污染返回 200/302)被选中,
    /// 但实际下载失败。若 selected 命中后直接 return 错误,镜像永远不被尝试,
    /// 表现为"竞速仍只走官方"。修复:selected 失败后清空并落到全源竞速。
    #[tokio::test]
    async fn test_download_falls_back_to_race_when_selected_source_fails() {
        // 官方源 probe 成功(伪成功)但下载失败
        let primary = Arc::new(
            MockProtocol::new()
                .with_probe_meta(Ok(FileMetadata {
                    file_name: "model.bin".into(),
                    file_size: Some(100),
                    content_type: None,
                    supports_range: true,
                    etag: None,
                    last_modified: None,
                }))
                .with_download_data(Err("primary download blocked".into())),
        );

        // 镜像 probe 成功,下载也成功(只接受镜像 URL)
        let mirror = Arc::new(
            MockProtocol::new()
                .with_probe_meta(Ok(FileMetadata {
                    file_name: "model.bin".into(),
                    file_size: Some(100),
                    content_type: None,
                    supports_range: true,
                    etag: None,
                    last_modified: None,
                }))
                .with_download_data(Ok(Bytes::from_static(b"from mirror")))
                .with_expected_url("http://mirror.com/file"),
        );

        let mirror_protocol =
            MirrorProtocol::new(primary, vec![("http://mirror.com/file".into(), mirror)]);

        // probe:官方先返回成功被选中(selected = 官方源)
        let meta = mirror_protocol.probe("http://primary.com/file").await;
        assert!(meta.is_ok(), "probe 应成功(官方伪成功)");

        // 下载:官方(选中源)失败后,必须回退到镜像竞速,而非直接报错
        let result = mirror_protocol
            .download_range("http://primary.com/file", 0, 99)
            .await;
        assert!(
            result.is_ok(),
            "选中源下载失败应回退全源竞速,实际错误: {:?}",
            result.err()
        );
        assert_eq!(result.unwrap(), Bytes::from_static(b"from mirror"));
    }
}
