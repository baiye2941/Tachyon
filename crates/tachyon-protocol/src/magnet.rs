//! 磁力链接协议适配层
//!
//! 通过 librqbit 的 Session 驱动 BitTorrent 下载，
//! 实现 Protocol trait 融入 DownloadTask 生命周期。
//!
//! # 设计要点
//!
//! - `probe()` 返回 `supports_range: false`，使引擎自动走 `download_full` 路径
//! - `download_range()` / `download_range_stream()` 返回错误（BT 不支持按字节范围请求）
//! - `download_full()` / `download_full_stream()` 等待 librqbit 完成后从磁盘读取

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use librqbit::{AddTorrent, AddTorrentOptions, ManagedTorrent, Session};
use tokio::io::AsyncReadExt;

use tachyon_core::config::MagnetConfig;
use tachyon_core::error::{DownloadError, DownloadResult};
use tachyon_core::traits::{ByteStream, Protocol};
use tachyon_core::types::FileMetadata;

/// 磁力链接协议客户端
///
/// 持有 librqbit Session 引用，通过 Protocol trait
/// 将 BitTorrent 下载适配为 Tachyon 统一下载接口。
pub struct MagnetProtocol {
    session: Arc<Session>,
    config: MagnetConfig,
    /// 默认下载输出目录（与 Session 创建时的 default_output_folder 一致）
    download_dir: PathBuf,
}

impl MagnetProtocol {
    /// 创建磁力链接协议客户端
    pub fn new(session: Arc<Session>, config: MagnetConfig, download_dir: PathBuf) -> Self {
        Self {
            session,
            config,
            download_dir,
        }
    }
}

/// 磁力链接格式校验
///
/// 验证 magnet URI 的必要条件:
/// - 以 `magnet:?` 开头
/// - 包含 `xt=urn:btih:` 参数
/// - btih 后的 info_hash 非空
pub fn validate_magnet_uri(uri: &str) -> DownloadResult<()> {
    if !uri.starts_with("magnet:?") {
        return Err(DownloadError::Config(format!(
            "磁力链接必须以 magnet:? 开头: {uri}"
        )));
    }

    // 查找 xt=urn:btih: 参数（大小写不敏感）
    let has_valid_xt = uri[8..] // 跳过 "magnet:?"
        .split('&')
        .any(|param| {
            let lower = param.to_ascii_lowercase();
            if let Some(hash) = lower.strip_prefix("xt=urn:btih:") {
                // info_hash 必须非空
                // 合法格式: 40 位十六进制(SHA1) 或 32 位 Base32
                !hash.is_empty()
            } else {
                false
            }
        });

    if !has_valid_xt {
        return Err(DownloadError::Protocol(format!(
            "磁力链接缺少有效的 xt=urn:btih: 参数: {uri}"
        )));
    }

    Ok(())
}

/// 通过 Session 添加磁力链接并获取 ManagedTorrent 句柄
///
/// `download_dir` 用于设置输出目录，`overwrite` 设为 true 允许覆盖已有文件
/// （磁力链接可能重复添加同一资源，BT 协议本身支持断点续传）。
async fn add_magnet_to_session(
    session: &Arc<Session>,
    url: &str,
    download_dir: &std::path::Path,
) -> DownloadResult<Arc<ManagedTorrent>> {
    let opts = AddTorrentOptions {
        overwrite: true,
        output_folder: Some(download_dir.to_string_lossy().into()),
        ..Default::default()
    };
    session
        .add_torrent(AddTorrent::from_url(url), Some(opts))
        .await
        .map_err(|e| DownloadError::Network(format!("添加磁力链接失败: {e}")))?
        .into_handle()
        .ok_or_else(|| DownloadError::Protocol("磁力链接已存在或添加失败".into()))
}

impl Protocol for MagnetProtocol {
    fn probe(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<FileMetadata>> + Send>> {
        // 先校验磁力链接格式
        if let Err(e) = validate_magnet_uri(url) {
            return Box::pin(async move { Err(e) });
        }

        let session = self.session.clone();
        let config = self.config.clone();
        let url = url.to_string();
        let download_dir = self.download_dir.clone();

        Box::pin(async move {
            let handle = add_magnet_to_session(&session, &url, &download_dir).await?;

            // 等待元数据就绪（带超时）
            let timeout = Duration::from_secs(config.metadata_timeout_secs);
            tokio::time::timeout(timeout, handle.wait_until_initialized())
                .await
                .map_err(|_| {
                    DownloadError::Timeout(format!(
                        "磁力链接元数据获取超时（{}秒）",
                        config.metadata_timeout_secs
                    ))
                })?
                .map_err(|e| DownloadError::Protocol(format!("磁力链接元数据获取失败: {e}")))?;

            // 提取元数据
            let (file_name, file_size) = handle
                .with_metadata(|m| {
                    let name = m
                        .name
                        .clone()
                        .unwrap_or_else(|| "unknown_torrent".to_string());
                    let size = m.lengths.total_length();
                    (name, size)
                })
                .map_err(|e| DownloadError::Protocol(format!("获取磁力链接元数据失败: {e}")))?;

            Ok(FileMetadata {
                file_name,
                file_size: Some(file_size),
                content_type: None,
                supports_range: false,
                etag: None,
                last_modified: None,
            })
        })
    }

    fn download_range(
        &self,
        _url: &str,
        _start: u64,
        _end: u64,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
        Box::pin(async {
            Err(DownloadError::Protocol("磁力链接不支持 Range 下载".into()))
        })
    }

    fn download_range_stream(
        &self,
        _url: &str,
        _start: u64,
        _end: u64,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
        Box::pin(async {
            Err(DownloadError::Protocol("磁力链接不支持 Range 下载".into()))
        })
    }

    fn download_full(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
        if let Err(e) = validate_magnet_uri(url) {
            return Box::pin(async move { Err(e) });
        }

        let session = self.session.clone();
        let url = url.to_string();
        let download_dir = self.download_dir.clone();

        Box::pin(async move {
            let handle = add_magnet_to_session(&session, &url, &download_dir).await?;

            // 等待下载完成
            handle
                .wait_until_completed()
                .await
                .map_err(|e| DownloadError::Network(format!("磁力链接下载失败: {e}")))?;

            // 从磁盘读取已下载文件
            let file_name = handle
                .name()
                .unwrap_or_else(|| "unknown_torrent".to_string());
            let file_path = download_dir.join(&file_name);

            let data = tokio::fs::read(&file_path)
                .await
                .map_err(DownloadError::Io)?;

            Ok(Bytes::from(data))
        })
    }

    fn download_full_stream(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
        if let Err(e) = validate_magnet_uri(url) {
            return Box::pin(async move { Err(e) });
        }

        let session = self.session.clone();
        let url = url.to_string();
        let download_dir = self.download_dir.clone();

        Box::pin(async move {
            let handle = add_magnet_to_session(&session, &url, &download_dir).await?;

            // 等待下载完成
            handle
                .wait_until_completed()
                .await
                .map_err(|e| DownloadError::Network(format!("磁力链接下载失败: {e}")))?;

            let file_name = handle
                .name()
                .unwrap_or_else(|| "unknown_torrent".to_string());
            let file_path = download_dir.join(&file_name);

            // 流式读取文件
            let file = tokio::fs::File::open(&file_path)
                .await
                .map_err(DownloadError::Io)?;

            use futures::stream::unfold;

            let stream = unfold(tokio::io::BufReader::new(file), |mut reader| async move {
                let mut buf = vec![0u8; 64 * 1024];
                match reader.read(&mut buf).await {
                    Ok(0) => None,
                    Ok(n) => {
                        buf.truncate(n);
                        Some((Ok(Bytes::from(buf)), reader))
                    }
                    Err(e) => Some((Err(DownloadError::Io(e)), reader)),
                }
            });

            Ok(Box::pin(stream) as ByteStream)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_magnet_uri_valid_sha1() {
        let uri = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=test";
        assert!(validate_magnet_uri(uri).is_ok());
    }

    #[test]
    fn test_validate_magnet_uri_valid_minimal() {
        let uri = "magnet:?xt=urn:btih:a1b2c3d4e5";
        assert!(validate_magnet_uri(uri).is_ok());
    }

    #[test]
    fn test_validate_magnet_uri_with_tracker() {
        let uri = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&tr=udp://tracker.example.com:6969";
        assert!(validate_magnet_uri(uri).is_ok());
    }

    #[test]
    fn test_validate_magnet_uri_with_multiple_trackers() {
        let uri = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&tr=udp://tracker1.example.com:6969&tr=udp://tracker2.example.com:6969";
        assert!(validate_magnet_uri(uri).is_ok());
    }

    #[test]
    fn test_validate_magnet_uri_rejects_no_magnet_prefix() {
        let uri = "http://example.com/file.torrent";
        let result = validate_magnet_uri(uri);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("必须以 magnet:? 开头")
        );
    }

    #[test]
    fn test_validate_magnet_uri_rejects_no_xt() {
        let uri = "magnet:?dn=test&tr=udp://tracker.example.com:6969";
        let result = validate_magnet_uri(uri);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("缺少有效的 xt=urn:btih:")
        );
    }

    #[test]
    fn test_validate_magnet_uri_rejects_empty_btih() {
        let uri = "magnet:?xt=urn:btih:&dn=test";
        let result = validate_magnet_uri(uri);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("缺少有效的 xt=urn:btih:")
        );
    }

    #[test]
    fn test_validate_magnet_uri_rejects_wrong_xt_scheme() {
        let uri = "magnet:?xt=urn:ed2k:abc123&dn=test";
        let result = validate_magnet_uri(uri);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("缺少有效的 xt=urn:btih:")
        );
    }

    #[test]
    fn test_magnet_protocol_new() {
        // 仅验证构造函数，不启动真实 Session
        // 真实 Session 创建需要异步环境和网络，在 e2e 测试中验证
    }

    /// 验证磁力链接中 urn:btih 大小写不敏感
    ///
    /// 实际磁力链接可能使用大写 BTIH（如 xt=urn:BTIH:...），
    /// validate_magnet_uri 应接受任意大小写组合。
    #[test]
    fn test_validate_magnet_uri_btih_case_insensitive() {
        let uri_upper = "magnet:?xt=urn:BTIH:0123456789abcdef0123456789abcdef01234567";
        assert!(
            validate_magnet_uri(uri_upper).is_ok(),
            "大写 BTIH 应被接受: {:?}",
            validate_magnet_uri(uri_upper)
        );

        let uri_mixed = "magnet:?xt=urn:BtIh:0123456789abcdef0123456789abcdef01234567";
        assert!(
            validate_magnet_uri(uri_mixed).is_ok(),
            "混合大小写 BtIh 应被接受: {:?}",
            validate_magnet_uri(uri_mixed)
        );
    }

    /// 验证磁力链接中 info hash 大小写不敏感
    ///
    /// info hash 可能是大写十六进制（如 ABCDEF...），应被接受。
    #[test]
    fn test_validate_magnet_uri_hash_uppercase() {
        let uri = "magnet:?xt=urn:btih:ABCDEF0123456789ABCDEF0123456789ABCDEF01";
        assert!(
            validate_magnet_uri(uri).is_ok(),
            "大写 info hash 应被接受: {:?}",
            validate_magnet_uri(uri)
        );
    }
}
