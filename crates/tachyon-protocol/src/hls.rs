//! HLS(m3u8)流媒体协议实现
//!
//! 支持:
//! - master playlist 解析(选最高码率 variant)
//! - media playlist 解析(提取 TS 分片 URI 列表)
//! - AES-128 解密(EXT-X-KEY)
//! - 通过现有 HttpClient 下载分片并合并为连续字节流
//!
//! 参考:RFC 8216 (HTTP Live Streaming)
//! 竞品:FluxDown 完整支持 HLS(AES-decrypt)+ DASH

use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use futures::Future;
use tachyon_core::safety::extract_filename;
use tachyon_core::traits::{ByteStream, Protocol};
use tachyon_core::types::FileMetadata;
use tachyon_core::{DownloadError, DownloadResult};

use crate::HttpClient;

// ── 数据类型 ────────────────────────────────────────────────────────

/// HLS 加密方法
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncryptionMethod {
    /// 无加密
    None,
    /// AES-128-CBC 加密(KEY URI + IV)
    Aes128,
    /// SAMPLE-AES(不常用,暂不支持解密)
    SampleAes,
}

/// EXT-X-KEY 标签描述的加密信息
#[derive(Debug, Clone)]
pub struct EncryptionKey {
    pub method: EncryptionMethod,
    pub uri: Option<String>,
    pub iv: Option<String>,
}

/// media playlist 中的一个分片
#[derive(Debug, Clone)]
pub struct MediaSegment {
    pub uri: String,
    pub duration: f64,
    pub encryption: Option<EncryptionKey>,
}

/// master playlist 中的一个 variant 流
#[derive(Debug, Clone)]
pub struct VariantStream {
    pub uri: String,
    pub bandwidth: u64,
    pub resolution: Option<String>,
}

/// 解析后的 m3u8 playlist
#[derive(Debug, Clone)]
pub enum Playlist {
    Master {
        variants: Vec<VariantStream>,
    },
    Media {
        segments: Vec<MediaSegment>,
        encryption: Option<EncryptionKey>,
        is_vod: bool,
        /// FIX-18.1:EXT-X-MEDIA-SEQUENCE 值(缺省 0)。AES-128 未显式 IV 时,
        /// IV = media_sequence + segment_index(RFC 8216 §4.3.2.4)。
        media_sequence: u64,
    },
}

// ── m3u8 解析 ───────────────────────────────────────────────────────

/// 解析 m3u8 文本为 Playlist
///
/// 支持两种 playlist:
/// - master playlist:含 EXT-X-STREAM-INF,列出多个 variant(按 bandwidth 降序)
/// - media playlist:含 EXTINF + 分片 URI,可选 EXT-X-KEY 加密信息
///
/// # 错误
/// - 缺少 `#EXTM3U` 头 -> `Protocol` 错误
/// - `EXT-X-KEY` 的 `METHOD` 非法 -> `Protocol` 错误
pub fn parse_m3u8(content: &str, base_url: Option<&str>) -> DownloadResult<Playlist> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("#EXTM3U") {
        return Err(DownloadError::Protocol(
            "无效的 m3u8:缺少 #EXTM3U 头".into(),
        ));
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut variants: Vec<VariantStream> = Vec::new();
    let mut segments: Vec<MediaSegment> = Vec::new();
    let mut global_encryption: Option<EncryptionKey> = None;
    let mut current_encryption: Option<EncryptionKey> = None;
    let mut is_vod = false;
    let mut pending_duration: Option<f64> = None;
    let mut pending_variant: Option<VariantStream> = None;
    // FIX-18.1:EXT-X-MEDIA-SEQUENCE(缺省 0),供 AES-128 IV 计算
    let mut media_sequence: u64 = 0;

    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let attrs = parse_attributes(rest);
            let bandwidth = attrs
                .get("BANDWIDTH")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            let resolution = attrs.get("RESOLUTION").cloned();
            pending_variant = Some(VariantStream {
                uri: String::new(),
                bandwidth,
                resolution,
            });
        } else if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let duration_str = rest.split(',').next().unwrap_or("0");
            pending_duration = duration_str.trim().parse::<f64>().ok();
        } else if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            let attrs = parse_attributes(rest);
            let method = match attrs.get("METHOD").map(|s| s.as_str()) {
                Some("NONE") => EncryptionMethod::None,
                Some("AES-128") => EncryptionMethod::Aes128,
                Some("SAMPLE-AES") => EncryptionMethod::SampleAes,
                _ => {
                    return Err(DownloadError::Protocol(format!(
                        "未知的 EXT-X-KEY METHOD: {line}"
                    )));
                }
            };
            let uri = attrs.get("URI").cloned();
            // FIX-18.2:密钥 URI 必须相对播放列表 base_url 解析(与分片 URI 一致),
            // 否则相对密钥地址(如 URI="key.bin")会被 reqwest::Url::parse 拒绝。
            let uri = match uri {
                Some(u) => Some(resolve_uri(&u, base_url)?),
                None => None,
            };
            let iv = attrs.get("IV").cloned();
            let key = EncryptionKey {
                method: method.clone(),
                uri,
                iv,
            };
            if method == EncryptionMethod::None {
                current_encryption = None;
            } else {
                current_encryption = Some(key.clone());
                if global_encryption.is_none() {
                    global_encryption = Some(key);
                }
            }
        } else if line.starts_with("#EXT-X-ENDLIST") {
            is_vod = true;
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            // FIX-18.1:解析媒体序列号(供 AES-128 IV 计算用)
            media_sequence = rest.trim().parse::<u64>().unwrap_or(0);
        } else if !line.starts_with('#') {
            let resolved_uri = resolve_uri(line, base_url)?;
            if let Some(ref mut variant) = pending_variant {
                variant.uri = resolved_uri;
                variants.push(pending_variant.take().unwrap());
            } else if let Some(duration) = pending_duration.take() {
                segments.push(MediaSegment {
                    uri: resolved_uri,
                    duration,
                    encryption: current_encryption.clone(),
                });
            }
        }
    }

    if !variants.is_empty() {
        variants.sort_by_key(|v| std::cmp::Reverse(v.bandwidth));
        Ok(Playlist::Master { variants })
    } else {
        Ok(Playlist::Media {
            segments,
            encryption: global_encryption,
            is_vod,
            media_sequence,
        })
    }
}

/// 解析属性字符串 `KEY=VALUE,KEY2=VALUE2` 为 HashMap
///
/// VALUE 可能用引号包裹(如 `URI="https://..."`),引号内的逗号不作为分隔符
fn parse_attributes(s: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let mut key = String::new();
    let mut value = String::new();
    let mut in_value = false;
    let mut in_quotes = false;

    for c in s.chars() {
        match c {
            '=' if !in_value && !in_quotes => {
                in_value = true;
            }
            '"' if in_value => {
                in_quotes = !in_quotes;
            }
            ',' if !in_quotes => {
                if !key.is_empty() {
                    map.insert(key.trim().to_string(), value.trim().to_string());
                }
                key.clear();
                value.clear();
                in_value = false;
            }
            _ => {
                if in_value {
                    value.push(c);
                } else {
                    key.push(c);
                }
            }
        }
    }
    if !key.is_empty() {
        map.insert(key.trim().to_string(), value.trim().to_string());
    }
    map
}

/// 将相对 URI 解析为绝对 URI(基于 base_url)
fn resolve_uri(uri: &str, base_url: Option<&str>) -> DownloadResult<String> {
    if uri.starts_with("http://") || uri.starts_with("https://") {
        return Ok(uri.to_string());
    }
    match base_url {
        Some(base) => {
            let base_url = reqwest::Url::parse(base)
                .map_err(|e| DownloadError::Protocol(format!("无效的 base_url: {e}")))?;
            base_url
                .join(uri)
                .map(|u| u.to_string())
                .map_err(|e| DownloadError::Protocol(format!("URI 解析失败: {e}")))
        }
        None => Ok(uri.to_string()),
    }
}

// ── HLS 协议实现 ────────────────────────────────────────────────────

/// AES-128-CBC 解密 HLS 加密分片
///
/// # 参数
/// - `http`: 用于下载密钥的 HTTP 客户端
/// - `key`: EXT-X-KEY 加密信息(含密钥 URI + IV)
/// - `data`: 加密的分片数据
/// - `seq`: 分片序号(无 IV 时用作默认 IV)
///
/// # IV 规则
/// - `key.iv` 为 `Some("0x...")` 时,解析为 16 字节大端 IV
/// - `key.iv` 为 `None` 时,使用分片序号作为 IV(大端填充 16 字节)
///
/// # PKCS7 去填充
/// AES-128-CBC 使用 PKCS7 填充,解密后自动去除填充字节
async fn decrypt_aes128(
    http: &Arc<HttpClient>,
    key: &EncryptionKey,
    data: &[u8],
    seq: u128,
) -> DownloadResult<Bytes> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
    type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

    // 下载密钥(16 字节)
    let key_uri = key
        .uri
        .as_ref()
        .ok_or_else(|| DownloadError::Protocol("AES-128 密钥缺少 URI".into()))?;
    let key_bytes = http.get_bytes(key_uri).await?;
    if key_bytes.len() != 16 {
        return Err(DownloadError::Protocol(format!(
            "AES-128 密钥长度非法: 预期 16 字节, 实际 {}",
            key_bytes.len()
        )));
    }
    let mut key_arr = [0u8; 16];
    key_arr.copy_from_slice(&key_bytes[..16]);

    // 解析 IV
    let iv: [u8; 16] = if let Some(iv_str) = &key.iv {
        // IV 格式: 0x<32 hex chars>
        let hex_str = iv_str
            .strip_prefix("0x")
            .or_else(|| iv_str.strip_prefix("0X"))
            .unwrap_or(iv_str);
        let iv_bytes = hex::decode(hex_str)
            .map_err(|e| DownloadError::Protocol(format!("IV hex 解析失败: {e}")))?;
        if iv_bytes.len() != 16 {
            return Err(DownloadError::Protocol(format!(
                "IV 长度非法: 预期 16 字节, 实际 {}",
                iv_bytes.len()
            )));
        }
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&iv_bytes);
        iv
    } else {
        // 无 IV 时,使用分片序号作为 IV(大端填充 16 字节)
        let mut iv = [0u8; 16];
        iv[0..16].copy_from_slice(&seq.to_be_bytes());
        iv
    };

    // AES-128-CBC 解密 + PKCS7 去填充
    let mut buf = data.to_vec();
    if buf.is_empty() || !buf.len().is_multiple_of(16) {
        return Err(DownloadError::Protocol(format!(
            "加密数据长度不是 16 的倍数: {}",
            buf.len()
        )));
    }
    let plaintext = Aes128CbcDec::new(&key_arr.into(), &iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| DownloadError::Protocol(format!("AES-128-CBC 解密失败: {e:?}")))?;
    Ok(Bytes::from(plaintext.to_vec()))
}

/// HLS 协议客户端
pub struct HlsProtocol {
    http: Arc<HttpClient>,
}

impl HlsProtocol {
    pub fn new(http: Arc<HttpClient>) -> Self {
        Self { http }
    }

    /// 获取 master playlist 中最高码率的 variant URI
    pub fn select_best_variant(playlist: &Playlist) -> Option<&str> {
        match playlist {
            Playlist::Master { variants } => variants.first().map(|v| v.uri.as_str()),
            _ => None,
        }
    }

    /// 获取并解析 playlist,若是 master 则跟随到最高码率 variant
    ///
    /// 返回 `(media_playlist, base_url)`
    async fn fetch_media_playlist(
        self: &Arc<Self>,
        url: &str,
    ) -> DownloadResult<(Playlist, String)> {
        let content = self.http.get_text(url, &[]).await?;
        let playlist = parse_m3u8(&content, Some(url))?;
        match playlist {
            Playlist::Master { .. } => {
                let best = Self::select_best_variant(&playlist)
                    .ok_or_else(|| DownloadError::Protocol("master playlist 无 variant".into()))?;
                // 解析为绝对 URI(parse_m3u8 已基于 base_url 解析)
                let best_uri = resolve_uri(best, Some(url))?;
                let content = self.http.get_text(&best_uri, &[]).await?;
                let media = parse_m3u8(&content, Some(&best_uri))?;
                match media {
                    Playlist::Media { .. } => Ok((media, best_uri)),
                    _ => Err(DownloadError::Protocol(
                        "variant URI 不是 media playlist".into(),
                    )),
                }
            }
            Playlist::Media { .. } => Ok((playlist, url.to_string())),
        }
    }
}

impl Protocol for HlsProtocol {
    fn probe(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<FileMetadata>> + Send>> {
        let this = Arc::clone(&self.http);
        let url = url.to_string();
        Box::pin(async move {
            let hls = Arc::new(HlsProtocol::new(this));
            let (playlist, _) = hls.fetch_media_playlist(&url).await?;
            match playlist {
                Playlist::Media { .. } => {
                    // FIX-18.9:HLS 无法预知精确字节数。旧实现用 "总时长 * 假设 5Mbps" 估算
                    // 并写入 file_size:Some(estimate),但引擎 execute_full_download 把
                    // Some(file_size) 当作精确 EOF 后置条件(pos != expected_size -> Err),
                    // 导致码率估算与真实长度不符时正确下载被误判失败。
                    // 正确做法:file_size 为 None(大小未知),引擎对未知大小走宽松完成路径。
                    let file_name = extract_filename(&url, None);
                    Ok(FileMetadata {
                        file_name,
                        file_size: None,
                        content_type: Some("video/mp2t".to_string()),
                        supports_range: false,
                        etag: None,
                        last_modified: None,
                        file_layout: None,
                        protocol_managed_storage: false,
                    })
                }
                _ => Err(DownloadError::Protocol(
                    "probe 后仍为 master playlist".into(),
                )),
            }
        })
    }

    fn download_range(
        &self,
        _url: &str,
        _start: u64,
        _end: u64,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
        Box::pin(async {
            Err(DownloadError::Protocol(
                "HLS 不支持字节级 Range 下载,请使用 download_full_stream".into(),
            ))
        })
    }

    fn download_range_stream(
        &self,
        _url: &str,
        _start: u64,
        _end: u64,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
        Box::pin(async {
            Err(DownloadError::Protocol(
                "HLS 不支持字节级 Range 下载,请使用 download_full_stream".into(),
            ))
        })
    }

    fn download_full(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
        let this = Arc::clone(&self.http);
        let url = url.to_string();
        Box::pin(async move {
            let hls = Arc::new(HlsProtocol::new(this));
            let (playlist, _) = hls.fetch_media_playlist(&url).await?;
            match playlist {
                Playlist::Media {
                    segments,
                    media_sequence,
                    ..
                } => {
                    let mut buf = Vec::new();
                    for (i, seg) in segments.iter().enumerate() {
                        let data = hls.http.get_bytes(&seg.uri).await?;
                        // FIX-18.3:download_full 必须与 download_full_stream 一致地对
                        // AES-128 分片解密(旧实现直接拼接密文,两个 API 结果不同)。
                        let data = match &seg.encryption {
                            Some(key) => match key.method {
                                EncryptionMethod::Aes128 => {
                                    decrypt_aes128(
                                        &hls.http,
                                        key,
                                        &data,
                                        // FIX-18.1:IV 序号 = media_sequence + 分片索引
                                        media_sequence as u128 + i as u128,
                                    )
                                    .await?
                                }
                                EncryptionMethod::None => data,
                                EncryptionMethod::SampleAes => {
                                    return Err(DownloadError::Protocol(
                                        "SAMPLE-AES 加密不支持".into(),
                                    ));
                                }
                            },
                            None => data,
                        };
                        buf.extend_from_slice(&data);
                    }
                    Ok(Bytes::from(buf))
                }
                _ => Err(DownloadError::Protocol("不是 media playlist".into())),
            }
        })
    }

    fn download_full_stream(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
        let this = Arc::clone(&self.http);
        let url = url.to_string();
        Box::pin(async move {
            let hls = Arc::new(HlsProtocol::new(this));
            let (playlist, _) = hls.fetch_media_playlist(&url).await?;
            match playlist {
                Playlist::Media {
                    segments,
                    media_sequence,
                    ..
                } => {
                    let http = Arc::clone(&hls.http);
                    let total = segments.len();
                    // 收集每个分片的 (uri, encryption) 信息,传入 unfold state
                    let seg_info: Vec<(String, Option<EncryptionKey>)> = segments
                        .iter()
                        .map(|s| (s.uri.clone(), s.encryption.clone()))
                        .collect();
                    let idx = 0usize;
                    // 使用 unfold 逐分片下载,避免 async_stream 依赖
                    // state 持 http + (uri, encryption) 列表 + idx + media_sequence,避免借用 segments
                    let stream = futures::stream::unfold(
                        (http, seg_info, idx, total, media_sequence),
                        |(http, segs, i, total, media_sequence)| async move {
                            if i >= total {
                                None
                            } else {
                                let (uri, encryption) = &segs[i];
                                match http.get_bytes(uri).await {
                                    Ok(data) => {
                                        // AES-128-CBC 解密(若分片已加密)
                                        let data = match encryption {
                                            Some(key) => match key.method {
                                                EncryptionMethod::Aes128 => {
                                                    match decrypt_aes128(
                                                        &http,
                                                        key,
                                                        &data,
                                                        // FIX-18.1:IV 序号 = media_sequence + 分片索引(RFC 8216 §4.3.2.4)
                                                        media_sequence as u128 + i as u128,
                                                    )
                                                    .await
                                                    {
                                                        Ok(d) => d,
                                                        Err(e) => {
                                                            return Some((
                                                                Err(e),
                                                                (
                                                                    http,
                                                                    segs,
                                                                    total,
                                                                    total,
                                                                    media_sequence,
                                                                ),
                                                            ));
                                                        }
                                                    }
                                                }
                                                EncryptionMethod::None => data,
                                                EncryptionMethod::SampleAes => {
                                                    return Some((
                                                        Err(DownloadError::Protocol(
                                                            "SAMPLE-AES 加密暂不支持".into(),
                                                        )),
                                                        (http, segs, total, total, media_sequence),
                                                    ));
                                                }
                                            },
                                            None => data,
                                        };
                                        Some((Ok(data), (http, segs, i + 1, total, media_sequence)))
                                    }
                                    Err(e) => {
                                        Some((Err(e), (http, segs, total, total, media_sequence)))
                                    }
                                }
                            }
                        },
                    );
                    Ok(Box::pin(stream) as ByteStream)
                }
                _ => Err(DownloadError::Protocol("不是 media playlist".into())),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_media_playlist() {
        let content = "#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:10
#EXTINF:10.000,
segment0.ts
#EXTINF:10.000,
segment1.ts
#EXTINF:5.500,
segment2.ts
#EXT-X-ENDLIST
";
        let playlist = parse_m3u8(content, None).expect("应解析成功");
        match playlist {
            Playlist::Media {
                segments,
                is_vod,
                encryption,
                ..
            } => {
                assert_eq!(segments.len(), 3, "应解析 3 个分片");
                assert_eq!(segments[0].uri, "segment0.ts");
                assert!((segments[0].duration - 10.0).abs() < 0.001);
                assert_eq!(segments[1].uri, "segment1.ts");
                assert!((segments[2].duration - 5.5).abs() < 0.001);
                assert!(is_vod, "含 EXT-X-ENDLIST 应为 VOD");
                assert!(encryption.is_none(), "无 EXT-X-KEY 应无加密");
            }
            _ => panic!("应为 Media playlist"),
        }
    }

    #[test]
    fn test_parse_rejects_missing_extm3u() {
        let content = "#EXT-X-VERSION:3\n#EXTINF:10,\nseg.ts\n";
        let result = parse_m3u8(content, None);
        assert!(result.is_err(), "缺少 #EXTM3U 头应报错");
    }

    #[test]
    fn test_parse_master_playlist_picks_highest_bandwidth() {
        let content = "#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=640x360
low.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=3000000,RESOLUTION=1280x720
mid.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080
high.m3u8
";
        let playlist = parse_m3u8(content, None).expect("应解析成功");
        match playlist {
            Playlist::Master { variants } => {
                assert_eq!(variants.len(), 3, "应有 3 个 variant");
                assert_eq!(variants[0].bandwidth, 5000000);
                assert_eq!(variants[0].uri, "high.m3u8");
                assert_eq!(variants[0].resolution.as_deref(), Some("1920x1080"));
                assert_eq!(variants[2].bandwidth, 1000000);
            }
            _ => panic!("应为 Master playlist"),
        }
    }

    #[test]
    fn test_parse_media_playlist_with_aes128() {
        let content = r#"#EXTM3U
#EXT-X-VERSION:3
#EXT-X-KEY:METHOD=AES-128,URI="https://cdn.example.com/key.bin",IV=0x00000000000000000000000000000001
#EXTINF:10.000,
segment0.ts
#EXTINF:10.000,
segment1.ts
#EXT-X-ENDLIST
"#;
        let playlist = parse_m3u8(content, None).expect("应解析成功");
        match playlist {
            Playlist::Media {
                segments,
                encryption,
                ..
            } => {
                assert_eq!(segments.len(), 2);
                assert!(encryption.is_some(), "应有全局加密信息");
                let key = encryption.unwrap();
                assert_eq!(key.method, EncryptionMethod::Aes128);
                assert_eq!(key.uri.as_deref(), Some("https://cdn.example.com/key.bin"));
                assert_eq!(
                    key.iv.as_deref(),
                    Some("0x00000000000000000000000000000001")
                );
                assert!(segments[0].encryption.is_some());
            }
            _ => panic!("应为 Media playlist"),
        }
    }

    // ── FIX-18.2: EXT-X-KEY 的相对 URI 必须相对播放列表解析 ──

    /// FIX-18.2:旧实现仅对分片 URI 调用 resolve_uri,EXT-X-KEY 的 URI 原样保留,
    /// 导致相对密钥地址(如 URI="key.bin")无法被 http.get_bytes 解析(reqwest::Url::parse 拒绝相对 URL)。
    /// 修复后密钥 URI 也应相对 base_url 解析。
    #[test]
    fn test_parse_resolves_relative_key_uri() {
        let content = r#"#EXTM3U
#EXT-X-VERSION:3
#EXT-X-KEY:METHOD=AES-128,URI="key.bin"
#EXTINF:10.000,
segment0.ts
#EXT-X-ENDLIST
"#;
        let base = "https://cdn.example.com/playlist/";
        let playlist = parse_m3u8(content, Some(base)).expect("应解析成功");
        match playlist {
            Playlist::Media {
                segments,
                encryption,
                ..
            } => {
                assert_eq!(
                    segments[0].uri,
                    "https://cdn.example.com/playlist/segment0.ts"
                );
                let key = encryption.expect("应有加密信息");
                assert_eq!(
                    key.uri.as_deref(),
                    Some("https://cdn.example.com/playlist/key.bin"),
                    "相对密钥 URI 必须相对播放列表 base_url 解析"
                );
            }
            _ => panic!("应为 Media playlist"),
        }
    }

    // ── FIX-18.1: 解析 EXT-X-MEDIA-SEQUENCE 用于 AES-128 IV ──

    /// FIX-18.1:RFC 8216 规定 AES-128 未显式给出 IV 时使用媒体序列号(EXT-X-MEDIA-SEQUENCE)。
    /// 旧实现不解析该标签,download_full_stream 用本地从 0 开始的分片索引作 IV,导致非 0 起始的
    /// 加密分片解密错误。修复后 parse_m3u8 解析 EXT-X-MEDIA-SEQUENCE 并存入 Playlist::Media,
    /// 供解密时使用 media_sequence + segment_index 作 IV。
    #[test]
    fn test_parse_media_sequence() {
        let content = r#"#EXTM3U
#EXT-X-VERSION:3
#EXT-X-MEDIA-SEQUENCE:5
#EXTINF:10.000,
segment0.ts
#EXTINF:10.000,
segment1.ts
#EXT-X-ENDLIST
"#;
        let playlist = parse_m3u8(content, None).expect("应解析成功");
        match playlist {
            Playlist::Media { media_sequence, .. } => {
                assert_eq!(media_sequence, 5, "应解析 EXT-X-MEDIA-SEQUENCE");
            }
            _ => panic!("应为 Media playlist"),
        }
    }

    #[test]
    fn test_parse_media_sequence_defaults_to_zero() {
        let content = r#"#EXTM3U
#EXT-X-VERSION:3
#EXTINF:10.000,
segment0.ts
#EXT-X-ENDLIST
"#;
        let playlist = parse_m3u8(content, None).expect("应解析成功");
        match playlist {
            Playlist::Media { media_sequence, .. } => {
                assert_eq!(media_sequence, 0, "缺省 EXT-X-MEDIA-SEQUENCE 应为 0");
            }
            _ => panic!("应为 Media playlist"),
        }
    }

    #[test]
    fn test_parse_aes128_key_method_none_clears_encryption() {
        let content = r#"#EXTM3U
#EXT-X-KEY:METHOD=AES-128,URI="https://cdn.example.com/key.bin"
#EXTINF:10.000,
encrypted.ts
#EXT-X-KEY:METHOD=NONE
#EXTINF:10.000,
plain.ts
#EXT-X-ENDLIST
"#;
        let playlist = parse_m3u8(content, None).expect("应解析成功");
        match playlist {
            Playlist::Media { segments, .. } => {
                assert_eq!(segments.len(), 2);
                assert!(segments[0].encryption.is_some());
                assert!(
                    segments[1].encryption.is_none(),
                    "METHOD=NONE 后的分片应无加密"
                );
            }
            _ => panic!("应为 Media playlist"),
        }
    }

    #[test]
    fn test_resolve_relative_uri_with_base_url() {
        let content = "#EXTM3U\n#EXTINF:10,\nsub/segment0.ts\n#EXT-X-ENDLIST\n";
        let playlist =
            parse_m3u8(content, Some("https://cdn.example.com/playlist.m3u8")).expect("应解析成功");
        match playlist {
            Playlist::Media { segments, .. } => {
                assert_eq!(segments[0].uri, "https://cdn.example.com/sub/segment0.ts");
            }
            _ => panic!("应为 Media playlist"),
        }
    }

    #[test]
    fn test_select_best_variant() {
        let playlist = parse_m3u8(
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000000\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=5000000\nhigh.m3u8\n",
            None,
        )
        .expect("应解析成功");
        let best = HlsProtocol::select_best_variant(&playlist);
        assert_eq!(best, Some("high.m3u8"));
    }

    #[test]
    fn test_select_best_variant_returns_none_for_media_playlist() {
        let playlist =
            parse_m3u8("#EXTM3U\n#EXTINF:10,\nseg.ts\n#EXT-X-ENDLIST\n", None).expect("应解析成功");
        assert!(HlsProtocol::select_best_variant(&playlist).is_none());
    }

    #[test]
    fn test_parse_attributes_handles_quoted_commas() {
        let attrs = parse_attributes(r#"METHOD=AES-128,URI="https://a.com/key?x=1,2",IV=0x01"#);
        assert_eq!(attrs.get("METHOD").map(|s| s.as_str()), Some("AES-128"));
        assert_eq!(
            attrs.get("URI").map(|s| s.as_str()),
            Some("https://a.com/key?x=1,2")
        );
        assert_eq!(attrs.get("IV").map(|s| s.as_str()), Some("0x01"));
    }

    #[test]
    fn test_parse_empty_media_playlist() {
        let content = "#EXTM3U\n#EXT-X-ENDLIST\n";
        let playlist = parse_m3u8(content, None).expect("应解析成功");
        match playlist {
            Playlist::Media {
                segments, is_vod, ..
            } => {
                assert!(segments.is_empty());
                assert!(is_vod);
            }
            _ => panic!("应为 Media playlist"),
        }
    }

    #[test]
    fn test_parse_rejects_unknown_key_method() {
        let content = "#EXTM3U\n#EXT-X-KEY:METHOD=UNKNOWN\n#EXTINF:10,\nseg.ts\n";
        let result = parse_m3u8(content, None);
        assert!(result.is_err(), "未知 METHOD 应报错");
    }

    // ── Protocol trait 集成测试(wiremock mock 服务器) ───────────────

    use tachyon_core::traits::Protocol;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    /// 构建 HLS mock 服务器:返回 media playlist + 2 个 TS 分片
    async fn setup_hls_mock() -> (wiremock::MockServer, String) {
        let server = wiremock::MockServer::start().await;
        let base = server.uri();
        let m3u8 = "#EXTM3U\n#EXT-X-VERSION:3\n#EXTINF:10.000,\nsegment0.ts\n#EXTINF:10.000,\nsegment1.ts\n#EXT-X-ENDLIST\n";
        Mock::given(method("GET"))
            .and(path("/playlist.m3u8"))
            .respond_with(ResponseTemplate::new(200).set_body_string(m3u8))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/segment0.ts"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"SEGMENT0_DATA".to_vec()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/segment1.ts"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"SEGMENT1_DATA".to_vec()))
            .mount(&server)
            .await;
        (server, format!("{base}/playlist.m3u8"))
    }

    #[tokio::test]
    async fn test_hls_probe_returns_metadata() {
        let (_server, url) = setup_hls_mock().await;
        // 需 test-harness feature 放行 loopback SSRF
        let http = HttpClient::with_timeouts(5, 10, None).unwrap();
        let hls = HlsProtocol::new(Arc::new(http));
        let meta = hls.probe(&url).await.expect("probe 应成功");
        assert!(!meta.supports_range, "HLS 不应支持 Range");
        assert_eq!(meta.content_type.as_deref(), Some("video/mp2t"));
        // FIX-18.9:HLS 无法预知精确字节数(码率估算 != 真实长度)。probe 不得把估算值
        // 放入 file_size,否则引擎 execute_full_download 会把 Some(file_size) 当作
        // 精确 EOF 后置条件(pos != expected_size -> Err),导致正确下载被误判失败。
        assert!(
            meta.file_size.is_none(),
            "HLS probe 的 file_size 必须为 None(大小未知),不得用码率估算值"
        );
    }

    #[tokio::test]
    async fn test_hls_download_full_stream_concatenates_segments() {
        let (_server, url) = setup_hls_mock().await;
        let http = HttpClient::with_timeouts(5, 10, None).unwrap();
        let hls = HlsProtocol::new(Arc::new(http));
        let stream = hls.download_full_stream(&url).await.expect("流应建立成功");
        let mut collected = Vec::new();
        let mut stream = stream;
        while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
            let chunk = chunk.expect("分片不应出错");
            collected.extend_from_slice(&chunk);
        }
        assert_eq!(&collected, b"SEGMENT0_DATASEGMENT1_DATA");
    }

    #[tokio::test]
    async fn test_hls_download_range_returns_error() {
        let (_server, url) = setup_hls_mock().await;
        let http = HttpClient::with_timeouts(5, 10, None).unwrap();
        let hls = HlsProtocol::new(Arc::new(http));
        let result = hls.download_range(&url, 0, 100).await;
        assert!(result.is_err(), "HLS download_range 应返回错误");
    }

    #[tokio::test]
    async fn test_hls_master_playlist_follows_best_variant() {
        let server = wiremock::MockServer::start().await;
        let base = server.uri();
        // master playlist
        let master = format!(
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000000\n{base}/low.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=5000000\n{base}/high.m3u8\n"
        );
        Mock::given(method("GET"))
            .and(path("/master.m3u8"))
            .respond_with(ResponseTemplate::new(200).set_body_string(master))
            .mount(&server)
            .await;
        // high.m3u8 media playlist
        let media = "#EXTM3U\n#EXTINF:5.000,\nseg.ts\n#EXT-X-ENDLIST\n".to_string();
        Mock::given(method("GET"))
            .and(path("/high.m3u8"))
            .respond_with(ResponseTemplate::new(200).set_body_string(media))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/low.m3u8"))
            .respond_with(ResponseTemplate::new(200).set_body_string("INVALID".to_string()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/seg.ts"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"TSDATA".to_vec()))
            .mount(&server)
            .await;

        let http = HttpClient::with_timeouts(5, 10, None).unwrap();
        let hls = HlsProtocol::new(Arc::new(http));
        let stream = hls
            .download_full_stream(&format!("{base}/master.m3u8"))
            .await
            .expect("流应建立成功");
        let mut collected = Vec::new();
        let mut stream = stream;
        while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
            collected.extend_from_slice(&chunk.expect("不应出错"));
        }
        assert_eq!(&collected, b"TSDATA");
        // 验证 high.m3u8 被请求,low.m3u8 未被请求
        // (wiremock 自动验证请求计数)
    }

    #[tokio::test]
    async fn test_hls_aes128_decryption() {
        use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
        type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

        let server = wiremock::MockServer::start().await;
        let base = server.uri();

        // 密钥 + IV(16 字节)
        let key: [u8; 16] = [0x42; 16];
        let iv: [u8; 16] = [0x00; 16];

        // 加密两个分片("HELLO_WORLD" 和 "TS_DATA!!!")
        let plaintext1 = b"HELLO_WORLD"; // 11 字节 -> PKCS7 填充到 16
        let plaintext2 = b"TS_DATA!!!"; // 10 字节 -> PKCS7 填充到 16

        let mut buf1 = vec![0u8; 16];
        buf1[..plaintext1.len()].copy_from_slice(plaintext1);
        let cipher1 = Aes128CbcEnc::new(&key.into(), &iv.into())
            .encrypt_padded_mut::<Pkcs7>(&mut buf1, plaintext1.len())
            .unwrap()
            .to_vec();

        // 第二个分片用 seq=1 作为 IV(无显式 IV 时)
        let mut iv2 = [0u8; 16];
        iv2[0..16].copy_from_slice(&1u128.to_be_bytes());
        let mut buf2 = vec![0u8; 16];
        buf2[..plaintext2.len()].copy_from_slice(plaintext2);
        let cipher2 = Aes128CbcEnc::new(&key.into(), &iv2.into())
            .encrypt_padded_mut::<Pkcs7>(&mut buf2, plaintext2.len())
            .unwrap()
            .to_vec();

        // 带 EXT-X-KEY 的 m3u8(无 IV,用分片序号作为 IV)
        let m3u8 = format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-KEY:METHOD=AES-128,URI=\"{base}/key.bin\"\n#EXTINF:10.000,\nsegment0.ts\n#EXTINF:10.000,\nsegment1.ts\n#EXT-X-ENDLIST\n"
        );

        Mock::given(method("GET"))
            .and(path("/playlist.m3u8"))
            .respond_with(ResponseTemplate::new(200).set_body_string(m3u8))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/key.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(key.to_vec()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/segment0.ts"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(cipher1))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/segment1.ts"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(cipher2))
            .mount(&server)
            .await;

        let http = HttpClient::with_timeouts(5, 10, None).unwrap();
        let hls = HlsProtocol::new(Arc::new(http));
        let stream = hls
            .download_full_stream(&format!("{base}/playlist.m3u8"))
            .await
            .expect("流应建立成功");
        let mut collected = Vec::new();
        let mut stream = stream;
        while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
            collected.extend_from_slice(&chunk.expect("不应出错"));
        }
        // 应等于 plaintext1 + plaintext2 (去填充后)
        assert_eq!(&collected, b"HELLO_WORLDTS_DATA!!!");
    }

    #[tokio::test]
    async fn test_hls_aes128_decryption_with_explicit_iv() {
        use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
        type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

        let server = wiremock::MockServer::start().await;
        let base = server.uri();

        let key: [u8; 16] = [0xAB; 16];
        let iv: [u8; 16] = [0x11; 16];
        let iv_hex = "0x11111111111111111111111111111111";

        let plaintext = b"DECRYPTED!!!"; // 12 字节 -> PKCS7 填充到 16
        let mut buf = vec![0u8; 16];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let cipher = Aes128CbcEnc::new(&key.into(), &iv.into())
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .unwrap()
            .to_vec();

        let m3u8 = format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-KEY:METHOD=AES-128,URI=\"{base}/key.bin\",IV={iv_hex}\n#EXTINF:10.000,\nseg.ts\n#EXT-X-ENDLIST\n"
        );

        Mock::given(method("GET"))
            .and(path("/playlist.m3u8"))
            .respond_with(ResponseTemplate::new(200).set_body_string(m3u8))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/key.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(key.to_vec()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/seg.ts"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(cipher))
            .mount(&server)
            .await;

        let http = HttpClient::with_timeouts(5, 10, None).unwrap();
        let hls = HlsProtocol::new(Arc::new(http));
        let stream = hls
            .download_full_stream(&format!("{base}/playlist.m3u8"))
            .await
            .expect("流应建立成功");
        let mut collected = Vec::new();
        let mut stream = stream;
        while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
            collected.extend_from_slice(&chunk.expect("不应出错"));
        }
        assert_eq!(&collected, b"DECRYPTED!!!");
    }

    /// FIX-18.3 回归:download_full 必须与 download_full_stream 一致地对 AES-128 分片解密。
    /// 旧实现 download_full 直接拼接密文(不调用 decrypt_aes128),导致两个 API 结果不同。
    #[tokio::test]
    async fn test_hls_download_full_decrypts_aes128_like_stream() {
        use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
        type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

        let server = wiremock::MockServer::start().await;
        let base = server.uri();

        let key: [u8; 16] = [0xAB; 16];
        let iv: [u8; 16] = [0x11; 16];
        let iv_hex = "0x11111111111111111111111111111111";

        let plaintext = b"DECRYPTED!!!"; // 12 字节 -> PKCS7 填充到 16
        let mut buf = vec![0u8; 16];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let cipher = Aes128CbcEnc::new(&key.into(), &iv.into())
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .unwrap()
            .to_vec();

        let m3u8 = format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-KEY:METHOD=AES-128,URI=\"{base}/key.bin\",IV={iv_hex}\n#EXTINF:10.000,\nseg.ts\n#EXT-X-ENDLIST\n"
        );

        Mock::given(method("GET"))
            .and(path("/playlist.m3u8"))
            .respond_with(ResponseTemplate::new(200).set_body_string(m3u8))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/key.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(key.to_vec()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/seg.ts"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(cipher.clone()))
            .mount(&server)
            .await;

        let http = HttpClient::with_timeouts(5, 10, None).unwrap();
        let hls = HlsProtocol::new(Arc::new(http));
        // download_full 应返回解密后的明文,而非拼接的密文
        let result = hls
            .download_full(&format!("{base}/playlist.m3u8"))
            .await
            .expect("download_full 应成功");
        assert_eq!(
            result.as_ref(),
            plaintext,
            "download_full 必须解密 AES-128 分片,不得返回密文"
        );
        assert_ne!(
            result.as_ref(),
            cipher.as_slice(),
            "结果不得等于密文(旧 bug:download_full 直接拼接密文)"
        );
    }
}
