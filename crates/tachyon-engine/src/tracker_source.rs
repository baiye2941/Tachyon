//! trackers.run 默认 tracker 源
//!
//! 负责从 <https://trackers.run/api/all> 拉取公共 tracker 列表，
//! 按用户配置的协议 / IP 版本筛选、去重，并按需写入 `MagnetConfig`。

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
#[cfg(test)]
use tachyon_core::DownloadError;
use tachyon_core::DownloadResult;
use tachyon_core::config::{MagnetConfig, TrackerSourceFilters};
use tachyon_protocol::http::HttpClient;

/// trackers.run 默认源 URL
const TRACKERS_RUN_API_URL: &str = "https://trackers.run/api/all";

/// 自动刷新 TTL（秒）
pub const TRACKERS_RUN_TTL_SECS: u64 = 3600;

/// 刷新 tracker 列表命令的返回结果
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeedTrackersResult {
    /// 是否实际更新了配置中的 tracker 列表
    pub updated: bool,
    /// 更新后 tracker 列表长度
    pub count: usize,
}

/// 解析 trackers.run 原始文本并按条件过滤、去重。
///
/// 数据源格式：IPv4 列表与 IPv6 列表以 `\n<>\n` 分隔，每行一个 tracker URL。
/// 解析规则：
/// - 空行跳过；
/// - 无法解析为合法 URL 的行跳过；
/// - 仅保留 scheme 在 `filters.protocols` 中的 URL（大小写不敏感）；
/// - `include_ipv4` / `include_ipv6` 控制是否保留对应分段的 tracker；
/// - 结果按出现顺序去重。
pub fn parse_trackers_run(text: &str, filters: &TrackerSourceFilters) -> Vec<String> {
    let protocol_set: HashSet<String> = filters
        .protocols
        .iter()
        .map(|p| p.to_ascii_lowercase())
        .collect();

    let (ipv4_text, ipv6_text) = text.split_once("\n<>\n").unwrap_or((text, ""));

    let mut seen = HashSet::new();
    let mut result = Vec::new();

    if filters.include_ipv4 {
        collect_filtered(ipv4_text, &protocol_set, &mut seen, &mut result);
    }
    if filters.include_ipv6 {
        collect_filtered(ipv6_text, &protocol_set, &mut seen, &mut result);
    }

    result
}

fn collect_filtered(
    text: &str,
    protocol_set: &HashSet<String>,
    seen: &mut HashSet<String>,
    out: &mut Vec<String>,
) {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(url) = url::Url::parse(trimmed) else {
            continue;
        };
        let scheme = url.scheme().to_ascii_lowercase();
        if !protocol_set.contains(&scheme) {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
}

/// 使用 `HttpClient` 从 trackers.run 拉取全部 tracker 并按条件过滤。
pub async fn fetch_default_trackers(
    client: &HttpClient,
    filters: &TrackerSourceFilters,
) -> DownloadResult<Vec<String>> {
    let text = client.get_text(TRACKERS_RUN_API_URL, &[]).await?;
    Ok(parse_trackers_run(&text, filters))
}

/// 根据当前配置状态决定是否刷新 `MagnetConfig.trackers`。
///
/// 触发条件：
/// - `trackers` 为空时立即拉取；
/// - `trackers_auto_managed` 为 true 且距离上次成功更新超过 `TRACKERS_RUN_TTL_SECS` 时拉取；
/// - 其他情况不拉取，返回 `(false, 当前长度)`。
///
/// 拉取成功后会更新 `trackers`、`trackers_updated_at` 和 `trackers_auto_managed`。
/// 拉取失败不会修改 `config`，错误向上传播。
pub async fn seed_trackers(
    config: &mut MagnetConfig,
    client: &HttpClient,
    now: u64,
) -> DownloadResult<(bool, usize)> {
    let filters = config.tracker_source_filters.clone();
    seed_trackers_internal(config, now, || fetch_default_trackers(client, &filters)).await
}

async fn seed_trackers_internal<F, Fut>(
    config: &mut MagnetConfig,
    now: u64,
    fetch: F,
) -> DownloadResult<(bool, usize)>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = DownloadResult<Vec<String>>>,
{
    let should_fetch = config.trackers.is_empty()
        || (config.trackers_auto_managed
            && config
                .trackers_updated_at
                .is_none_or(|updated_at| now.saturating_sub(updated_at) >= TRACKERS_RUN_TTL_SECS));

    if !should_fetch {
        return Ok((false, config.trackers.len()));
    }

    let urls = fetch().await?;
    config.trackers = urls;
    config.trackers_updated_at = Some(now);
    config.trackers_auto_managed = true;

    Ok((true, config.trackers.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_text() -> &'static str {
        "udp://tracker.opentrackr.org:1337/announce\n\
         http://tracker.example.com:80/announce\n\
         https://tracker.example.com:443/announce\n\
         wss://tracker.example.com:443/announce\n\
         <>\n\
         https://tracker.manager.v6.navy:443/announce\n\
         udp://tracker.v6.example.org:1337/announce"
    }

    #[test]
    fn parse_keeps_all_by_default() {
        let filters = TrackerSourceFilters::default();
        let trackers = parse_trackers_run(sample_text(), &filters);
        assert_eq!(trackers.len(), 6);
        assert!(trackers.contains(&"udp://tracker.opentrackr.org:1337/announce".to_string()));
        assert!(trackers.contains(&"https://tracker.manager.v6.navy:443/announce".to_string()));
    }

    #[test]
    fn parse_filters_by_protocol() {
        let filters = TrackerSourceFilters {
            protocols: vec!["udp".to_string()],
            include_ipv4: true,
            include_ipv6: true,
        };
        let trackers = parse_trackers_run(sample_text(), &filters);
        assert_eq!(trackers.len(), 2);
        assert!(trackers.iter().all(|t| t.starts_with("udp://")));
    }

    #[test]
    fn parse_excludes_ipv6() {
        let filters = TrackerSourceFilters {
            protocols: vec!["udp".to_string(), "http".to_string(), "https".to_string()],
            include_ipv4: true,
            include_ipv6: false,
        };
        let trackers = parse_trackers_run(sample_text(), &filters);
        assert_eq!(trackers.len(), 3);
        assert!(
            trackers
                .iter()
                .all(|t| !t.contains("v6") && !t.contains("navy"))
        );
    }

    #[test]
    fn parse_excludes_ipv4() {
        let filters = TrackerSourceFilters {
            protocols: vec!["https".to_string()],
            include_ipv4: false,
            include_ipv6: true,
        };
        let trackers = parse_trackers_run(sample_text(), &filters);
        assert_eq!(trackers.len(), 1);
        assert_eq!(trackers[0], "https://tracker.manager.v6.navy:443/announce");
    }

    #[test]
    fn parse_handles_missing_ipv6_section() {
        let text = "udp://tracker.opentrackr.org:1337/announce\n<>\n";
        let filters = TrackerSourceFilters::default();
        let trackers = parse_trackers_run(text, &filters);
        assert_eq!(trackers.len(), 1);
    }

    #[test]
    fn parse_returns_empty_for_empty_input() {
        let filters = TrackerSourceFilters::default();
        assert!(parse_trackers_run("", &filters).is_empty());
        assert!(parse_trackers_run("   \n   \n", &filters).is_empty());
    }

    #[test]
    fn parse_deduplicates() {
        let text = "udp://tracker.opentrackr.org:1337/announce\nudp://tracker.opentrackr.org:1337/announce\n<>\nudp://tracker.opentrackr.org:1337/announce";
        let filters = TrackerSourceFilters::default();
        let trackers = parse_trackers_run(text, &filters);
        assert_eq!(trackers.len(), 1);
    }

    #[test]
    fn parse_skips_invalid_urls() {
        let text = "not-a-url\nudp://tracker.example.com:1337/announce\n";
        let filters = TrackerSourceFilters::default();
        let trackers = parse_trackers_run(text, &filters);
        assert_eq!(trackers.len(), 1);
        assert_eq!(trackers[0], "udp://tracker.example.com:1337/announce");
    }

    #[test]
    fn parse_protocol_filter_is_case_insensitive() {
        let text =
            "UDP://tracker.example.com:1337/announce\nhttps://tracker.example.com:443/announce";
        let filters = TrackerSourceFilters {
            protocols: vec!["udp".to_string()],
            include_ipv4: true,
            include_ipv6: true,
        };
        let trackers = parse_trackers_run(text, &filters);
        assert_eq!(trackers.len(), 1);
        assert!(trackers[0].starts_with("UDP://"));
    }

    #[tokio::test]
    #[allow(clippy::field_reassign_with_default)]
    async fn seed_fetches_when_trackers_empty() {
        let mut config = MagnetConfig::default();
        let result = seed_trackers_internal(&mut config, 1000, || async {
            Ok(vec!["udp://new.example.com:1337/announce".to_string()])
        })
        .await
        .unwrap();
        assert_eq!(result, (true, 1));
        assert_eq!(config.trackers.len(), 1);
        assert_eq!(config.trackers_updated_at, Some(1000));
        assert!(config.trackers_auto_managed);
    }

    #[tokio::test]
    #[allow(clippy::field_reassign_with_default)]
    async fn seed_skips_when_ttl_not_expired() {
        let mut config = MagnetConfig::default();
        config.trackers = vec!["udp://existing.example.com:1337/announce".to_string()];
        config.trackers_auto_managed = true;
        config.trackers_updated_at = Some(1000);

        let result =
            seed_trackers_internal(&mut config, 1000 + TRACKERS_RUN_TTL_SECS - 1, || async {
                Ok(vec!["udp://new.example.com:1337/announce".to_string()])
            })
            .await
            .unwrap();

        assert_eq!(result, (false, 1));
        assert_eq!(
            config.trackers[0],
            "udp://existing.example.com:1337/announce"
        );
    }

    #[tokio::test]
    #[allow(clippy::field_reassign_with_default)]
    async fn seed_fetches_when_ttl_expired() {
        let mut config = MagnetConfig::default();
        config.trackers = vec!["udp://existing.example.com:1337/announce".to_string()];
        config.trackers_auto_managed = true;
        config.trackers_updated_at = Some(1000);

        let result = seed_trackers_internal(&mut config, 1000 + TRACKERS_RUN_TTL_SECS, || async {
            Ok(vec![
                "udp://new1.example.com:1337/announce".to_string(),
                "udp://new2.example.com:1337/announce".to_string(),
            ])
        })
        .await
        .unwrap();

        assert_eq!(result, (true, 2));
        assert_eq!(
            config.trackers_updated_at,
            Some(1000 + TRACKERS_RUN_TTL_SECS)
        );
        assert!(config.trackers_auto_managed);
    }

    #[tokio::test]
    #[allow(clippy::field_reassign_with_default)]
    async fn seed_skips_when_not_auto_managed() {
        let mut config = MagnetConfig::default();
        config.trackers = vec!["udp://existing.example.com:1337/announce".to_string()];
        config.trackers_auto_managed = false;
        config.trackers_updated_at = Some(0);

        let result = seed_trackers_internal(&mut config, u64::MAX, || async {
            Ok(vec!["udp://new.example.com:1337/announce".to_string()])
        })
        .await
        .unwrap();

        assert_eq!(result, (false, 1));
        assert_eq!(
            config.trackers[0],
            "udp://existing.example.com:1337/announce"
        );
        assert!(!config.trackers_auto_managed);
    }

    #[tokio::test]
    #[allow(clippy::field_reassign_with_default)]
    async fn seed_preserves_trackers_on_fetch_error() {
        let mut config = MagnetConfig::default();
        config.trackers = vec!["udp://existing.example.com:1337/announce".to_string()];
        config.trackers_auto_managed = true;
        config.trackers_updated_at = Some(0);

        let result = seed_trackers_internal(&mut config, TRACKERS_RUN_TTL_SECS + 1, || async {
            Err(DownloadError::Network("service unavailable".into()))
        })
        .await;

        assert!(result.is_err());
        assert_eq!(config.trackers.len(), 1);
        assert_eq!(config.trackers_updated_at, Some(0));
    }

    #[tokio::test]
    #[allow(clippy::field_reassign_with_default)]
    async fn seed_sets_auto_managed_after_manual_refresh() {
        let mut config = MagnetConfig::default();
        config.trackers_auto_managed = false;
        config.trackers = vec!["udp://old.example.com:1337/announce".to_string()];

        // 空列表时才触发刷新，这里手动把 trackers 清空以模拟手动刷新入口
        config.trackers.clear();

        let result = seed_trackers_internal(&mut config, 42, || async {
            Ok(vec![
                "udp://refreshed.example.com:1337/announce".to_string(),
            ])
        })
        .await
        .unwrap();

        assert_eq!(result, (true, 1));
        assert!(config.trackers_auto_managed);
    }
}
