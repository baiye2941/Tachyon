//! 下载配置类型

use serde::{Deserialize, Serialize};

pub const USER_AGENT: &str = "Tachyon/0.1.0";

/// download_full (单请求全量下载) 的最大允许字节数
///
/// 超过此阈值的文件应使用分片下载(download_range)。
/// 用于统一 HTTP / QUIC / FTP 三协议的 OOM 防护上限。
pub const MAX_FULL_DOWNLOAD_SIZE: usize = 64 * 1024 * 1024; // 64MB

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
    /// Windows IOCP 异步 I/O 后端
    ///
    /// 仅在 Windows 上生效；非 Windows 平台自动回退到 Standard。
    Iocp,
    /// Linux io_uring 零拷贝后端（O_DIRECT + fixed buffer）
    ///
    /// 仅在 Linux 5.4+ 上生效；其他平台自动回退到 Standard。
    /// 提供零拷贝写入管道，绕过页缓存直接使用 fixed buffer。
    IoUring,
}

/// 分片并发数上限
///
/// 过高的并发可能导致源服务器拒绝服务或本地资源耗尽。
/// 256 个并发分片在千兆网络下可占满带宽。
pub const MAX_CONCURRENT_FRAGMENTS_LIMIT: u32 = 256;

/// 最大重试次数上限
///
/// 100 次重试足以覆盖指数退避策略下数小时的恢复窗口。
pub const MAX_RETRIES_LIMIT: u32 = 100;

/// 请求超时上限(秒)
///
/// 1 小时足以覆盖慢速源的大文件单分片传输。
pub const REQUEST_TIMEOUT_SECS_LIMIT: u64 = 3600;

/// 连接超时上限(秒)
///
/// 5 分钟涵盖高延迟网络(如卫星链路)的 TCP 握手时间。
pub const CONNECT_TIMEOUT_SECS_LIMIT: u64 = 300;

/// 暂停超时上限(秒)
///
/// 24 小时防止任务永久暂停占用资源。
pub const PAUSE_TIMEOUT_SECS_LIMIT: u64 = 86400;

/// 单主机最大连接数上限
///
/// 128 路连接在常规多线程 HTTP 客户端中已属较高水平,
/// 继续增大收益递减且增加端口耗尽风险。
pub const MAX_CONNECTIONS_PER_HOST_LIMIT: u32 = 128;

/// 全局最大连接数上限
///
/// 4096 足以支撑高并发下载场景,
/// 同时避免文件描述符耗尽。
pub const MAX_GLOBAL_CONNECTIONS_LIMIT: u32 = 4096;

/// 最大并发任务数上限
///
/// 100 个并发任务在高带宽场景下已足够,
/// 继续增大易导致调度开销激增。
pub const MAX_CONCURRENT_TASKS_LIMIT: u32 = 100;

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
    /// 未知大小整块流式下载的最大允许字节数
    #[serde(default = "default_max_full_stream_bytes")]
    pub max_full_stream_bytes: u64,
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
    #[serde(default = "default_max_full_stream_bytes")]
    max_full_stream_bytes: u64,
    #[serde(default)]
    io_strategy: IoStrategy,
}

fn default_pause_timeout_secs() -> u64 {
    300
}

fn default_connect_timeout_secs() -> u64 {
    10
}

pub const fn default_max_full_stream_bytes() -> u64 {
    64 * 1024 * 1024 * 1024
}

impl From<DownloadConfigSerde> for DownloadConfig {
    fn from(value: DownloadConfigSerde) -> Self {
        // 空列表也回退到默认值,防止显式传入 "authorizedDirs": [] 绕过路径检查
        let authorized_dirs = value
            .authorized_dirs
            .filter(|dirs| !dirs.is_empty())
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
            max_full_stream_bytes: value.max_full_stream_bytes,
            authorized_dirs,
            io_strategy: value.io_strategy,
        }
    }
}

impl Default for DownloadConfig {
    fn default() -> Self {
        let download_dir = dirs()
            .map(|d| d.join("Downloads").to_string_lossy().to_string())
            .unwrap_or_else(|| {
                // 回退到系统临时目录,而非当前工作目录
                std::env::temp_dir()
                    .join("tachyon-downloads")
                    .to_string_lossy()
                    .to_string()
            });
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
            max_full_stream_bytes: default_max_full_stream_bytes(),
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
    /// A-04: 高带宽阈值(字节/秒),超过此值时分片大小翻倍
    #[serde(default = "default_high_bw_threshold")]
    pub high_bandwidth_threshold: u64,
    /// A-04: 中等带宽阈值(字节/秒),超过此值时分片大小增加 50%
    #[serde(default = "default_medium_bw_threshold")]
    pub medium_bandwidth_threshold: u64,
}

fn default_high_bw_threshold() -> u64 {
    100 * 1024 * 1024 // 100 MiB/s
}

fn default_medium_bw_threshold() -> u64 {
    10 * 1024 * 1024 // 10 MiB/s
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
            high_bandwidth_threshold: default_high_bw_threshold(),
            medium_bandwidth_threshold: default_medium_bw_threshold(),
        }
    }
}

impl DownloadConfig {
    /// 校验配置值是否在合法范围内
    ///
    /// 反序列化不会校验数值边界,必须在使用前显式调用此方法。
    pub fn validate(&self) -> crate::DownloadResult<()> {
        let e = |msg: &str| crate::DownloadError::Config(msg.into());

        if self.max_concurrent_fragments == 0 {
            return Err(e("max_concurrent_fragments 必须 >= 1"));
        }
        if self.max_concurrent_fragments > MAX_CONCURRENT_FRAGMENTS_LIMIT {
            return Err(e(&format!(
                "max_concurrent_fragments 不能超过 {MAX_CONCURRENT_FRAGMENTS_LIMIT}"
            )));
        }
        if self.max_retries > MAX_RETRIES_LIMIT {
            return Err(e(&format!("max_retries 不能超过 {MAX_RETRIES_LIMIT}")));
        }
        if self.request_timeout_secs == 0 {
            return Err(e("request_timeout_secs 必须 >= 1"));
        }
        if self.request_timeout_secs > REQUEST_TIMEOUT_SECS_LIMIT {
            return Err(e(&format!(
                "request_timeout_secs 不能超过 {REQUEST_TIMEOUT_SECS_LIMIT}"
            )));
        }
        if self.connect_timeout_secs == 0 {
            return Err(e("connect_timeout_secs 必须 >= 1"));
        }
        if self.connect_timeout_secs > CONNECT_TIMEOUT_SECS_LIMIT {
            return Err(e(&format!(
                "connect_timeout_secs 不能超过 {CONNECT_TIMEOUT_SECS_LIMIT}"
            )));
        }
        if self.download_dir.is_empty() {
            return Err(e("download_dir 不能为空"));
        }
        if self.pause_timeout_secs == 0 {
            return Err(e("pause_timeout_secs 必须 >= 1"));
        }
        if self.pause_timeout_secs > PAUSE_TIMEOUT_SECS_LIMIT {
            return Err(e(&format!(
                "pause_timeout_secs 不能超过 {PAUSE_TIMEOUT_SECS_LIMIT} (24h)"
            )));
        }
        if let Some(rate) = self.rate_limit_bytes_per_sec
            && rate == 0
        {
            return Err(e("rate_limit_bytes_per_sec 不能为 0,使用 None 表示不限速"));
        }
        if self.max_full_stream_bytes == 0 {
            return Err(e("max_full_stream_bytes 必须 >= 1"));
        }
        if self.user_agent.is_empty() {
            return Err(e("user_agent 不能为空"));
        }
        if self.authorized_dirs.is_empty() {
            return Err(e("authorized_dirs 不能为空"));
        }
        Ok(())
    }
}

impl ConnectionConfig {
    /// 校验连接配置值是否在合法范围内
    pub fn validate(&self) -> crate::DownloadResult<()> {
        let e = |msg: &str| crate::DownloadError::Config(msg.into());

        if self.max_connections_per_host == 0 {
            return Err(e("max_connections_per_host 必须 >= 1"));
        }
        if self.max_connections_per_host > MAX_CONNECTIONS_PER_HOST_LIMIT {
            return Err(e(&format!(
                "max_connections_per_host 不能超过 {MAX_CONNECTIONS_PER_HOST_LIMIT}"
            )));
        }
        if self.max_global_connections == 0 {
            return Err(e("max_global_connections 必须 >= 1"));
        }
        if self.max_global_connections > MAX_GLOBAL_CONNECTIONS_LIMIT {
            return Err(e(&format!(
                "max_global_connections 不能超过 {MAX_GLOBAL_CONNECTIONS_LIMIT}"
            )));
        }
        Ok(())
    }
}

impl SchedulerConfig {
    /// 校验调度器配置值是否在合法范围内
    pub fn validate(&self) -> crate::DownloadResult<()> {
        let e = |msg: &str| crate::DownloadError::Config(msg.into());

        if self.min_fragment_size == 0 {
            return Err(e("min_fragment_size 必须 >= 1"));
        }
        if self.max_fragment_size == 0 {
            return Err(e("max_fragment_size 必须 >= 1"));
        }
        if self.min_fragment_size > self.max_fragment_size {
            return Err(e("min_fragment_size 不能大于 max_fragment_size"));
        }
        if !(0.0..=1.0).contains(&self.ewma_alpha) {
            return Err(e("ewma_alpha 必须在 0.0 ~ 1.0 之间"));
        }
        if self.default_target_fragments == 0 {
            return Err(e("default_target_fragments 必须 >= 1"));
        }
        if self.sampling_interval_secs == 0 {
            return Err(e("sampling_interval_secs 必须 >= 1"));
        }
        Ok(())
    }
}

impl AppConfig {
    /// 校验所有子配置
    pub fn validate(&self) -> crate::DownloadResult<()> {
        let e = |msg: &str| crate::DownloadError::Config(msg.into());

        if self.max_concurrent_tasks == 0 {
            return Err(e("max_concurrent_tasks 必须 >= 1"));
        }
        if self.max_concurrent_tasks > MAX_CONCURRENT_TASKS_LIMIT {
            return Err(e(&format!(
                "max_concurrent_tasks 不能超过 {MAX_CONCURRENT_TASKS_LIMIT}"
            )));
        }
        self.download.validate()?;
        self.connection.validate()?;
        self.scheduler.validate()?;
        Ok(())
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
        // dirs() 可用时包含 "Downloads"，否则回退到 temp_dir/tachyon-downloads
        let dir = config.download_dir();
        assert!(
            dir.contains("Downloads") || dir.contains("tachyon-downloads"),
            "unexpected download_dir: {dir}"
        );
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
        for strategy in [
            IoStrategy::Standard,
            IoStrategy::WinAligned,
            IoStrategy::Iocp,
        ] {
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
        assert_eq!(
            serde_json::from_str::<IoStrategy>("\"iocp\"").unwrap(),
            IoStrategy::Iocp
        );
    }

    #[test]
    fn test_io_strategy_iocp_serialization() {
        // 序列化为 camelCase
        assert_eq!(
            serde_json::to_string(&IoStrategy::Iocp).unwrap(),
            "\"iocp\""
        );
        // 反序列化
        let deserialized: IoStrategy = serde_json::from_str("\"iocp\"").unwrap();
        assert_eq!(deserialized, IoStrategy::Iocp);
        // 默认值不受影响
        assert_ne!(IoStrategy::Iocp, IoStrategy::default());
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
