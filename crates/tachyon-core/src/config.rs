//! 下载配置类型

use serde::{Deserialize, Serialize};

pub const USER_AGENT: &str = "Tachyon/0.1.0";

/// I/O 存储后端策略
///
/// 控制下载写入时使用的文件 I/O 后端。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IoStrategy {
    /// 标准 TokioFile 后端（跨平台稳定路径）
    #[default]
    Standard,
    /// Windows 优化后端（NO_BUFFERING + SEQUENTIAL_SCAN）
    ///
    /// 仅在 Windows 上生效；其他平台自动回退到 Standard。
    /// 要求写入偏移和长度对齐到 512 字节边界。
    WinAligned,
}

/// 下载配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", from = "DownloadConfigSerde")]
pub struct DownloadConfig {
    /// 下载目录
    pub download_dir: String,
    /// 最大并发分片数
    pub max_concurrent_fragments: u32,
    /// 最大重试次数
    pub max_retries: u32,
    /// 单次请求超时(秒)
    pub request_timeout_secs: u64,
    /// 连接建立超时(秒)
    pub connect_timeout_secs: u64,
    /// 是否启用校验
    pub verify_checksum: bool,
    /// 自定义 User-Agent
    pub user_agent: String,
    /// 自定义请求头
    pub headers: std::collections::HashMap<String, String>,
    /// 暂停状态最大持续时间(秒)
    pub pause_timeout_secs: u64,
    /// 后端允许写入的下载目录列表
    pub authorized_dirs: Vec<String>,
    /// 全局下载限速(字节/秒)，None 表示不限速
    #[serde(default)]
    pub rate_limit_bytes_per_sec: Option<u64>,
    /// I/O 存储后端策略
    #[serde(default)]
    pub io_strategy: IoStrategy,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DownloadConfigSerde {
    download_dir: String,
    max_concurrent_fragments: u32,
    max_retries: u32,
    request_timeout_secs: u64,
    #[serde(default = "default_connect_timeout_secs")]
    connect_timeout_secs: u64,
    verify_checksum: bool,
    user_agent: String,
    headers: std::collections::HashMap<String, String>,
    #[serde(default = "default_pause_timeout_secs")]
    pause_timeout_secs: u64,
    authorized_dirs: Option<Vec<String>>,
    #[serde(default)]
    rate_limit_bytes_per_sec: Option<u64>,
    #[serde(default)]
    io_strategy: IoStrategy,
}

fn default_pause_timeout_secs() -> u64 {
    300
}

fn default_connect_timeout_secs() -> u64 {
    10
}

impl From<DownloadConfigSerde> for DownloadConfig {
    fn from(value: DownloadConfigSerde) -> Self {
        let authorized_dirs = value
            .authorized_dirs
            .unwrap_or_else(|| vec![value.download_dir.clone()]);
        Self {
            download_dir: value.download_dir,
            max_concurrent_fragments: value.max_concurrent_fragments,
            max_retries: value.max_retries,
            request_timeout_secs: value.request_timeout_secs,
            connect_timeout_secs: value.connect_timeout_secs,
            verify_checksum: value.verify_checksum,
            user_agent: value.user_agent,
            headers: value.headers,
            pause_timeout_secs: value.pause_timeout_secs,
            rate_limit_bytes_per_sec: value.rate_limit_bytes_per_sec,
            authorized_dirs,
            io_strategy: value.io_strategy,
        }
    }
}

impl Default for DownloadConfig {
    fn default() -> Self {
        let download_dir = dirs()
            .map(|d| d.join("Downloads").to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());
        Self {
            download_dir: download_dir.clone(),
            max_concurrent_fragments: 16,
            max_retries: 3,
            request_timeout_secs: 30,
            connect_timeout_secs: 10,
            verify_checksum: true,
            user_agent: USER_AGENT.to_string(),
            headers: std::collections::HashMap::new(),
            pause_timeout_secs: 300,
            rate_limit_bytes_per_sec: None,
            authorized_dirs: vec![download_dir],
            io_strategy: IoStrategy::default(),
        }
    }
}

/// 连接配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionConfig {
    /// 单主机最大连接数
    pub max_connections_per_host: u32,
    /// 全局最大连接数
    pub max_global_connections: u32,
    /// Keep-Alive 超时(秒)
    pub keep_alive_timeout_secs: u64,
    /// 连接建立超时(秒)
    pub connect_timeout_secs: u64,
    /// 是否启用 HTTP/2
    pub enable_http2: bool,
    /// 是否启用 QUIC
    pub enable_quic: bool,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            max_connections_per_host: 16,
            max_global_connections: 256,
            keep_alive_timeout_secs: 30,
            connect_timeout_secs: 10,
            enable_http2: true,
            enable_quic: false,
        }
    }
}

/// 调度器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchedulerConfig {
    /// 最小分片大小(字节)
    pub min_fragment_size: u64,
    /// 最大分片大小(字节)
    pub max_fragment_size: u64,
    /// 带宽预测采样间隔(秒)
    pub sampling_interval_secs: u64,
    /// EWMA 平滑因子(0.0 ~ 1.0)
    pub ewma_alpha: f64,
    /// 默认目标分片数(无调度器建议时使用)
    #[serde(default = "default_target_fragments")]
    pub default_target_fragments: u32,
}

fn default_target_fragments() -> u32 {
    16
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            min_fragment_size: 1024 * 1024,      // 1MB
            max_fragment_size: 64 * 1024 * 1024, // 64MB
            sampling_interval_secs: 60,
            ewma_alpha: 0.3,
            default_target_fragments: 16,
        }
    }
}

pub fn dirs() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    /// 最大并发任务数
    pub max_concurrent_tasks: u32,
    /// 下载配置
    pub download: DownloadConfig,
    /// 连接配置
    pub connection: ConnectionConfig,
    /// 调度器配置
    pub scheduler: SchedulerConfig,
}

impl AppConfig {
    /// 获取默认下载目录(委托给 DownloadConfig)
    pub fn download_dir(&self) -> &str {
        &self.download.download_dir
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            max_concurrent_tasks: 5,
            download: DownloadConfig::default(),
            connection: ConnectionConfig::default(),
            scheduler: SchedulerConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_download_config_default() {
        let config = DownloadConfig::default();
        assert_eq!(config.max_concurrent_fragments, 16);
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.request_timeout_secs, 30);
        assert!(config.verify_checksum);
        assert!(config.user_agent.starts_with("Tachyon/"));
        assert!(config.headers.is_empty());
    }

    #[test]
    fn test_user_agent_constant() {
        assert_eq!(USER_AGENT, "Tachyon/0.1.0");
        assert_eq!(DownloadConfig::default().user_agent, USER_AGENT);
    }

    #[test]
    fn test_app_config_default() {
        let config = AppConfig::default();
        assert_eq!(config.max_concurrent_tasks, 5);
        // download_dir 现在委托给 DownloadConfig
        assert!(config.download_dir().contains("Downloads") || config.download_dir() == ".");
    }

    #[test]
    fn test_connection_config_default() {
        let config = ConnectionConfig::default();
        assert_eq!(config.max_connections_per_host, 16);
        assert_eq!(config.max_global_connections, 256);
        assert_eq!(config.keep_alive_timeout_secs, 30);
        assert_eq!(config.connect_timeout_secs, 10);
        assert!(config.enable_http2);
        assert!(!config.enable_quic);
    }

    #[test]
    fn test_scheduler_config_default() {
        let config = SchedulerConfig::default();
        assert_eq!(config.min_fragment_size, 1024 * 1024);
        assert_eq!(config.max_fragment_size, 64 * 1024 * 1024);
        assert_eq!(config.sampling_interval_secs, 60);
        assert!((config.ewma_alpha - 0.3).abs() < f64::EPSILON);
        assert_eq!(config.default_target_fragments, 16);
    }

    #[test]
    fn test_scheduler_config_deserializes_legacy_without_target_fragments() {
        let json = r#"{
            "minFragmentSize":1048576,
            "maxFragmentSize":67108864,
            "samplingIntervalSecs":60,
            "ewmaAlpha":0.3
        }"#;
        let config: SchedulerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.default_target_fragments, 16);
    }

    #[test]
    fn test_download_config_serialization() {
        let config = DownloadConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: DownloadConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.max_concurrent_fragments,
            config.max_concurrent_fragments
        );
    }

    #[test]
    fn test_download_config_pause_timeout_default() {
        let config = DownloadConfig::default();
        assert_eq!(config.pause_timeout_secs, 300);
    }

    #[test]
    fn test_download_config_rate_limit_default_is_none() {
        let config = DownloadConfig::default();
        assert_eq!(config.rate_limit_bytes_per_sec, None);
    }

    #[test]
    fn test_download_config_deserializes_with_rate_limit() {
        let json = r#"{
            "downloadDir":"/tmp",
            "maxConcurrentFragments":8,
            "maxRetries":3,
            "requestTimeoutSecs":60,
            "verifyChecksum":true,
            "userAgent":"Test",
            "headers":{},
            "rateLimitBytesPerSec":1048576
        }"#;
        let config: DownloadConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.rate_limit_bytes_per_sec, Some(1_048_576));
    }

    #[test]
    fn test_download_config_deserializes_without_rate_limit() {
        let json = r#"{
            "downloadDir":"/tmp",
            "maxConcurrentFragments":8,
            "maxRetries":3,
            "requestTimeoutSecs":60,
            "verifyChecksum":true,
            "userAgent":"Test",
            "headers":{}
        }"#;
        let config: DownloadConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.rate_limit_bytes_per_sec, None);
    }

    #[test]
    fn test_download_config_authorized_dirs_default_contains_download_dir() {
        let config = DownloadConfig::default();
        assert!(config.authorized_dirs.contains(&config.download_dir));
    }

    #[test]
    fn test_download_config_deserializes_legacy_json() {
        let json = r#"{
            "downloadDir":"/tmp/downloads",
            "maxConcurrentFragments":8,
            "maxRetries":5,
            "requestTimeoutSecs":60,
            "verifyChecksum":false,
            "userAgent":"Tachyon/Legacy",
            "headers":{}
        }"#;

        let config: DownloadConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.pause_timeout_secs, 300);
        assert_eq!(config.authorized_dirs, vec!["/tmp/downloads".to_string()]);
    }

    #[test]
    fn test_connection_config_serialization() {
        let config = ConnectionConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: ConnectionConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.max_connections_per_host,
            config.max_connections_per_host
        );
    }

    #[test]
    fn test_scheduler_config_serialization() {
        let config = SchedulerConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: SchedulerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.min_fragment_size, config.min_fragment_size);
        assert_eq!(
            deserialized.default_target_fragments,
            config.default_target_fragments
        );
    }

    #[test]
    fn test_io_strategy_default_is_standard() {
        assert_eq!(IoStrategy::default(), IoStrategy::Standard);
    }

    #[test]
    fn test_io_strategy_serialization_roundtrip() {
        for strategy in [IoStrategy::Standard, IoStrategy::WinAligned] {
            let json = serde_json::to_string(&strategy).unwrap();
            let deserialized: IoStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, strategy);
        }
    }

    #[test]
    fn test_io_strategy_deserializes_from_json() {
        assert_eq!(
            serde_json::from_str::<IoStrategy>("\"standard\"").unwrap(),
            IoStrategy::Standard
        );
        assert_eq!(
            serde_json::from_str::<IoStrategy>("\"winAligned\"").unwrap(),
            IoStrategy::WinAligned
        );
    }

    #[test]
    fn test_download_config_io_strategy_defaults_to_standard() {
        let json = r#"{
            "downloadDir":"/tmp",
            "maxConcurrentFragments":4,
            "maxRetries":3,
            "requestTimeoutSecs":30,
            "verifyChecksum":true,
            "userAgent":"Test",
            "headers":{}
        }"#;
        let config: DownloadConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.io_strategy, IoStrategy::Standard);
    }

    #[test]
    fn test_download_config_io_strategy_from_json() {
        let json = r#"{
            "downloadDir":"/tmp",
            "maxConcurrentFragments":4,
            "maxRetries":3,
            "requestTimeoutSecs":30,
            "verifyChecksum":true,
            "userAgent":"Test",
            "headers":{},
            "ioStrategy":"winAligned"
        }"#;
        let config: DownloadConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.io_strategy, IoStrategy::WinAligned);
    }
}
