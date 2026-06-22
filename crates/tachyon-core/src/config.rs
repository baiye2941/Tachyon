//! 下载配置类型

use serde::{Deserialize, Serialize};

pub const USER_AGENT: &str = "Tachyon/0.1.0";

/// download_full (单请求全量下载) 的最大允许字节数
///
/// 超过此阈值的文件应使用分片下载(download_range)。
/// 用于统一 HTTP / QUIC / FTP 三协议的 OOM 防护上限。
pub const MAX_FULL_DOWNLOAD_SIZE: usize = 64 * 1024 * 1024; // 64MB

/// 分片写入批大小阈值(字节)。网络 chunk 先累积到 write_buf,达到此阈值后
/// 批量刷写存储,减少 write_at 系统调用次数。256 KiB 在 HDD/SSD 与默认
/// 分片大小下均为合理折中,过小则 I/O 放大,过大则内存占用与尾块延迟上升。
///
/// 跨层公共常量:tachyon-engine 的分片写入循环、tachyon-app 构造全局
/// BufferPool 时的 buffer_size 均引用此值,保证池中 buffer 尺寸与 worker
/// 写入阈值一致,避免池化 buffer 与实际批大小错配。
pub const WRITE_BATCH_BYTES: usize = 256 * 1024;

/// I/O 存储后端策略
///
/// 控制下载写入时使用的文件 I/O 后端。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IoStrategy {
    /// 标准 TokioFile 后端（跨平台稳定路径）
    Standard,
    /// Windows 优化后端（NO_BUFFERING + SEQUENTIAL_SCAN）
    ///
    /// 仅在 Windows 上生效；其他平台自动回退到 Standard。
    /// 要求写入偏移和长度对齐到 512 字节边界。
    WinAligned,
    /// Windows IOCP 异步 I/O 后端
    ///
    /// 仅在 Windows 上生效；非 Windows 平台自动回退到 Standard。
    /// 使用无锁完成端口和 NO_BUFFERING 写入，提供高吞吐零页缓存路径。
    Iocp,
    /// Linux io_uring 零拷贝后端（O_DIRECT + fixed buffer）
    ///
    /// 仅在 Linux 5.4+ 上生效；其他平台自动回退到 Standard。
    /// 提供零拷贝写入管道，绕过页缓存直接使用 fixed buffer。
    IoUring,
}

/// Windows 平台默认使用 IOCP 后端（无锁完成端口 + NO_BUFFERING），
/// 其他平台默认使用 Standard（TokioFile）。
#[cfg(target_os = "windows")]
#[allow(clippy::derivable_impls)]
impl Default for IoStrategy {
    fn default() -> Self {
        IoStrategy::Iocp
    }
}

#[cfg(not(target_os = "windows"))]
impl Default for IoStrategy {
    fn default() -> Self {
        // Linux 5.4+ 自动检测 io_uring 可用性,可用时默认使用 io_uring 后端
        #[cfg(target_os = "linux")]
        {
            if is_io_uring_available() {
                return IoStrategy::IoUring;
            }
        }
        IoStrategy::Standard
    }
}

/// 检测当前内核是否支持 io_uring
///
/// 通过尝试创建最小 io_uring 实例来检测,失败则说明不可用。
/// 使用 raw syscall 而非 libc 封装(libc 0.2 不导出 io_uring_params/io_uring_setup)。
/// 检测结果不缓存(仅在进程启动时调用一次)。
///
/// Miri 不支持 raw syscall,直接返回 false。
#[cfg(target_os = "linux")]
fn is_io_uring_available() -> bool {
    #[cfg(miri)]
    {
        false
    }
    #[cfg(not(miri))]
    {
        // io_uring_setup syscall number: 425 (x86_64), 425 (aarch64)
        const SYS_IO_URING_SETUP: i64 = 425;
        // io_uring_params 结构体大小约 120 字节,用零初始化即可用于探测
        let mut params = [0u8; 128];
        unsafe {
            let ring_fd = libc::syscall(SYS_IO_URING_SETUP, 1u32, params.as_mut_ptr());
            if ring_fd >= 0 {
                libc::close(ring_fd as i32);
                return true;
            }
            false
        }
    }
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

/// 校验策略
///
/// 控制下载完成后是否以及如何校验文件完整性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VerifyStrategy {
    /// 必须有 expected hash 且校验通过,否则返回错误
    Require,
    /// 有 expected hash 时校验,无 hash 时跳过并记录日志(默认)
    #[default]
    BestEffort,
    /// 完全跳过校验
    Skip,
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
    /// 校验策略
    #[serde(default)]
    pub verify_strategy: VerifyStrategy,
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
    #[serde(default)]
    verify_strategy: VerifyStrategy,
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
            verify_strategy: value.verify_strategy,
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
            verify_strategy: VerifyStrategy::BestEffort,
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

/// 磁力链接下载配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagnetConfig {
    /// 元数据获取超时（秒），默认 120
    #[serde(default = "default_metadata_timeout_secs")]
    pub metadata_timeout_secs: u64,
    /// 下载超时（秒），默认 0（不限）
    #[serde(default)]
    pub download_timeout_secs: u64,
    /// 是否启用 DHT 协议（默认启用）
    ///
    /// DHT（分布式哈希表）是 BitTorrent 的去中心化节点发现协议，
    /// 启用后可脱离 tracker 发现 peer，显著提升磁力链接解析速度。
    #[serde(default = "default_true")]
    pub enable_dht: bool,
    /// 是否启用 UPnP 端口转发（默认启用）
    ///
    /// UPnP 可自动在路由器上开放监听端口，允许入站 peer 连接，加速下载。
    #[serde(default = "default_true")]
    pub enable_upnp: bool,
    /// 全局 tracker 服务器列表
    ///
    /// 这些 tracker 会附加到每个磁力链接的 tracker 列表中，
    /// 即使磁力链接本身不包含 tracker 也能快速发现 peer。
    /// 格式：`udp://host:port/announce` 或 `http://host:port/announce`
    #[serde(default)]
    pub trackers: Vec<String>,
}

fn default_metadata_timeout_secs() -> u64 {
    120
}

/// 布尔默认值 true 的辅助函数（serde default 不支持直接写 true）
fn default_true() -> bool {
    true
}

impl Default for MagnetConfig {
    fn default() -> Self {
        Self {
            metadata_timeout_secs: 120,
            download_timeout_secs: 0,
            enable_dht: true,
            enable_upnp: true,
            trackers: Vec::new(),
        }
    }
}

impl MagnetConfig {
    /// 校验配置值
    pub fn validate(&self) -> crate::DownloadResult<()> {
        let e = |msg: &str| crate::DownloadError::Config(msg.into());
        if self.metadata_timeout_secs == 0 {
            return Err(e("metadata_timeout_secs 必须 >= 1"));
        }
        Ok(())
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
    /// 带宽采样间隔(秒)—— **当前未生效**(保留字段,向后兼容)
    ///
    /// 带宽采样实际由"每分片完成"驱动(见 `downloader.rs` execute_fragmented_download
    /// 的 join 循环),而非定时器。此字段保留是为了不破坏已序列化的配置文件,
    /// 未来若改为定时采样则会启用。修改采样行为不应调整此值。
    #[serde(default = "default_sampling_interval_secs")]
    pub sampling_interval_secs: u64,
    /// EWMA 平滑因子(0.0 ~ 1.0)
    pub ewma_alpha: f64,
    /// Holt 趋势平滑因子(0.0 ~ 1.0)
    #[serde(default = "default_ewma_beta")]
    pub ewma_beta: f64,
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

/// SchedulerConfig.ewma_beta 的默认值
///
/// 保持与旧代码 `ewma_alpha * 0.3` 在 alpha=0.3 时的行为一致。
fn default_ewma_beta() -> f64 {
    0.09
}

/// SchedulerConfig.sampling_interval_secs 的默认值(当前未生效,保留兼容)
fn default_sampling_interval_secs() -> u64 {
    60
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            min_fragment_size: 1024 * 1024,      // 1MB
            max_fragment_size: 64 * 1024 * 1024, // 64MB
            sampling_interval_secs: 60,
            ewma_alpha: 0.3,
            ewma_beta: default_ewma_beta(),
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
        if !(0.0..=1.0).contains(&self.ewma_beta) {
            return Err(e("ewma_beta 必须在 0.0 ~ 1.0 之间"));
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

/// 配置白名单补丁 DTO
///
/// 仅包含允许前端修改的字段(Option 包裹,Some 表示更新,None 表示保留原值)。
/// 排除的安全字段:
/// - `headers`: 有 CRLF 注入风险,需独立授权流程
/// - `authorized_dirs`: 路径安全敏感,需独立授权流程
/// - `user_agent`: 不应被前端覆盖
/// - `max_full_stream_bytes`: OOM 防护上限,不应暴露给前端
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigPatch {
    pub max_concurrent_tasks: Option<u32>,
    pub download: Option<DownloadPatch>,
    pub connection: Option<ConnectionPatch>,
}

/// 下载配置白名单补丁
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadPatch {
    pub download_dir: Option<String>,
    pub max_concurrent_fragments: Option<u32>,
    pub max_retries: Option<u32>,
    pub request_timeout_secs: Option<u64>,
    pub connect_timeout_secs: Option<u64>,
    pub verify_checksum: Option<bool>,
    pub pause_timeout_secs: Option<u64>,
    pub rate_limit_bytes_per_sec: Option<Option<u64>>,
    pub io_strategy: Option<IoStrategy>,
}

/// 连接配置白名单补丁
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionPatch {
    pub max_connections_per_host: Option<u32>,
    pub max_global_connections: Option<u32>,
    pub keep_alive_timeout_secs: Option<u64>,
    pub connect_timeout_secs: Option<u64>,
    pub enable_http2: Option<bool>,
    pub enable_quic: Option<bool>,
}

impl ConfigPatch {
    /// 将补丁应用到现有 AppConfig,仅更新 Some 字段,保留其余不变
    ///
    /// 返回应用后的新 AppConfig(不修改原配置)。
    pub fn apply_to(&self, base: &AppConfig) -> AppConfig {
        let mut result = base.clone();
        if let Some(v) = self.max_concurrent_tasks {
            result.max_concurrent_tasks = v;
        }
        if let Some(patch) = &self.download {
            patch.apply_to(&mut result.download);
        }
        if let Some(patch) = &self.connection {
            patch.apply_to(&mut result.connection);
        }
        result
    }
}

impl DownloadPatch {
    /// 将下载补丁应用到现有 DownloadConfig,仅更新 Some 字段
    pub fn apply_to(&self, base: &mut DownloadConfig) {
        if let Some(v) = &self.download_dir {
            base.download_dir = v.clone();
        }
        if let Some(v) = self.max_concurrent_fragments {
            base.max_concurrent_fragments = v;
        }
        if let Some(v) = self.max_retries {
            base.max_retries = v;
        }
        if let Some(v) = self.request_timeout_secs {
            base.request_timeout_secs = v;
        }
        if let Some(v) = self.connect_timeout_secs {
            base.connect_timeout_secs = v;
        }
        if let Some(v) = self.verify_checksum {
            base.verify_checksum = v;
        }
        if let Some(v) = self.pause_timeout_secs {
            base.pause_timeout_secs = v;
        }
        if let Some(v) = &self.rate_limit_bytes_per_sec {
            base.rate_limit_bytes_per_sec = *v;
        }
        if let Some(v) = self.io_strategy {
            base.io_strategy = v;
        }
    }
}

impl ConnectionPatch {
    /// 将连接补丁应用到现有 ConnectionConfig,仅更新 Some 字段
    pub fn apply_to(&self, base: &mut ConnectionConfig) {
        if let Some(v) = self.max_connections_per_host {
            base.max_connections_per_host = v;
        }
        if let Some(v) = self.max_global_connections {
            base.max_global_connections = v;
        }
        if let Some(v) = self.keep_alive_timeout_secs {
            base.keep_alive_timeout_secs = v;
        }
        if let Some(v) = self.connect_timeout_secs {
            base.connect_timeout_secs = v;
        }
        if let Some(v) = self.enable_http2 {
            base.enable_http2 = v;
        }
        if let Some(v) = self.enable_quic {
            base.enable_quic = v;
        }
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
        self.magnet.validate()?;
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
    /// 磁力链接配置
    #[serde(default)]
    pub magnet: MagnetConfig,
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
            magnet: MagnetConfig::default(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn test_download_config_default() {
        let config = DownloadConfig::default();
        assert_eq!(config.max_concurrent_fragments, 16);
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.request_timeout_secs, 30);
        assert!(config.verify_checksum);
        assert_eq!(config.verify_strategy, VerifyStrategy::BestEffort);
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
    fn test_verify_strategy_default_is_best_effort() {
        assert_eq!(VerifyStrategy::default(), VerifyStrategy::BestEffort);
    }

    #[test]
    fn test_verify_strategy_serialization_roundtrip() {
        for strategy in [
            VerifyStrategy::Require,
            VerifyStrategy::BestEffort,
            VerifyStrategy::Skip,
        ] {
            let json = serde_json::to_string(&strategy).unwrap();
            let deserialized: VerifyStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, strategy);
        }
    }

    #[test]
    fn test_verify_strategy_deserializes_from_json() {
        assert_eq!(
            serde_json::from_str::<VerifyStrategy>("\"require\"").unwrap(),
            VerifyStrategy::Require
        );
        assert_eq!(
            serde_json::from_str::<VerifyStrategy>("\"bestEffort\"").unwrap(),
            VerifyStrategy::BestEffort
        );
        assert_eq!(
            serde_json::from_str::<VerifyStrategy>("\"skip\"").unwrap(),
            VerifyStrategy::Skip
        );
    }

    #[test]
    fn test_download_config_deserializes_legacy_without_verify_strategy() {
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
        // 旧配置无 verifyStrategy 时,默认应为 BestEffort
        assert_eq!(config.verify_strategy, VerifyStrategy::BestEffort);
    }

    #[test]
    fn test_download_config_deserializes_with_verify_strategy() {
        let json = r#"{
            "downloadDir":"/tmp",
            "maxConcurrentFragments":4,
            "maxRetries":3,
            "requestTimeoutSecs":30,
            "verifyChecksum":true,
            "verifyStrategy":"require",
            "userAgent":"Test",
            "headers":{}
        }"#;
        let config: DownloadConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.verify_strategy, VerifyStrategy::Require);
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
        // Windows 默认为 IOCP
        #[cfg(target_os = "windows")]
        assert_eq!(IoStrategy::default(), IoStrategy::Iocp);
        // Linux 5.4+ 默认为 IoUring(运行时检测),其他非 Windows 平台默认为 Standard
        #[cfg(target_os = "linux")]
        {
            let default = IoStrategy::default();
            assert!(
                default == IoStrategy::IoUring || default == IoStrategy::Standard,
                "Linux 默认应为 IoUring 或 Standard,实际: {default:?}"
            );
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
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
        // 非默认值的验证: Standard 不是 Iocp
        assert_ne!(IoStrategy::Standard, IoStrategy::Iocp);
    }

    #[test]
    fn test_download_config_io_strategy_defaults_to_platform() {
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
        // 缺少 ioStrategy 字段时使用平台默认值
        assert_eq!(config.io_strategy, IoStrategy::default());
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

    #[test]
    fn test_io_strategy_default_platform_specific() {
        let default = IoStrategy::default();
        #[cfg(target_os = "windows")]
        assert_eq!(default, IoStrategy::Iocp, "Windows 默认应为 IOCP");
        #[cfg(target_os = "linux")]
        assert!(
            default == IoStrategy::IoUring || default == IoStrategy::Standard,
            "Linux 默认应为 IoUring 或 Standard,实际: {default:?}"
        );
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        assert_eq!(
            default,
            IoStrategy::Standard,
            "非 Windows/Linux 默认应为 Standard"
        );
    }

    // -----------------------------------------------------------------------
    // P0: 配置校验边界测试
    // -----------------------------------------------------------------------

    fn assert_config_error<T: std::fmt::Debug>(result: crate::DownloadResult<T>, expected: &str) {
        match result.unwrap_err() {
            crate::DownloadError::Config(msg) => {
                assert!(
                    msg.contains(expected),
                    "错误消息应包含 {expected:?},实际: {msg:?}"
                )
            }
            other => panic!("预期 Config 错误,实际: {other:?}"),
        }
    }

    fn valid_download_config() -> DownloadConfig {
        let mut cfg = DownloadConfig::default();
        cfg.download_dir = std::env::temp_dir().to_string_lossy().to_string();
        cfg.authorized_dirs = vec![cfg.download_dir.clone()];
        cfg
    }

    #[test]
    fn test_download_config_validate_max_concurrent_fragments_zero() {
        let mut cfg = valid_download_config();
        cfg.max_concurrent_fragments = 0;
        assert_config_error(cfg.validate(), "max_concurrent_fragments 必须 >= 1");
    }

    #[test]
    fn test_download_config_validate_max_concurrent_fragments_over_limit() {
        let mut cfg = valid_download_config();
        cfg.max_concurrent_fragments = MAX_CONCURRENT_FRAGMENTS_LIMIT + 1;
        assert_config_error(cfg.validate(), "max_concurrent_fragments 不能超过");
    }

    #[test]
    fn test_download_config_validate_max_retries_over_limit() {
        let mut cfg = valid_download_config();
        cfg.max_retries = MAX_RETRIES_LIMIT + 1;
        assert_config_error(cfg.validate(), "max_retries 不能超过");
    }

    #[test]
    fn test_download_config_validate_request_timeout_zero() {
        let mut cfg = valid_download_config();
        cfg.request_timeout_secs = 0;
        assert_config_error(cfg.validate(), "request_timeout_secs 必须 >= 1");
    }

    #[test]
    fn test_download_config_validate_request_timeout_over_limit() {
        let mut cfg = valid_download_config();
        cfg.request_timeout_secs = REQUEST_TIMEOUT_SECS_LIMIT + 1;
        assert_config_error(cfg.validate(), "request_timeout_secs 不能超过");
    }

    #[test]
    fn test_download_config_validate_connect_timeout_zero() {
        let mut cfg = valid_download_config();
        cfg.connect_timeout_secs = 0;
        assert_config_error(cfg.validate(), "connect_timeout_secs 必须 >= 1");
    }

    #[test]
    fn test_download_config_validate_connect_timeout_over_limit() {
        let mut cfg = valid_download_config();
        cfg.connect_timeout_secs = CONNECT_TIMEOUT_SECS_LIMIT + 1;
        assert_config_error(cfg.validate(), "connect_timeout_secs 不能超过");
    }

    #[test]
    fn test_download_config_validate_empty_download_dir() {
        let mut cfg = valid_download_config();
        cfg.download_dir = String::new();
        assert_config_error(cfg.validate(), "download_dir 不能为空");
    }

    #[test]
    fn test_download_config_validate_pause_timeout_zero() {
        let mut cfg = valid_download_config();
        cfg.pause_timeout_secs = 0;
        assert_config_error(cfg.validate(), "pause_timeout_secs 必须 >= 1");
    }

    #[test]
    fn test_download_config_validate_pause_timeout_over_limit() {
        let mut cfg = valid_download_config();
        cfg.pause_timeout_secs = PAUSE_TIMEOUT_SECS_LIMIT + 1;
        assert_config_error(cfg.validate(), "pause_timeout_secs 不能超过");
    }

    #[test]
    fn test_download_config_validate_rate_limit_zero() {
        let mut cfg = valid_download_config();
        cfg.rate_limit_bytes_per_sec = Some(0);
        assert_config_error(cfg.validate(), "rate_limit_bytes_per_sec 不能为 0");
    }

    #[test]
    fn test_download_config_validate_max_full_stream_bytes_zero() {
        let mut cfg = valid_download_config();
        cfg.max_full_stream_bytes = 0;
        assert_config_error(cfg.validate(), "max_full_stream_bytes 必须 >= 1");
    }

    #[test]
    fn test_download_config_validate_empty_user_agent() {
        let mut cfg = valid_download_config();
        cfg.user_agent = String::new();
        assert_config_error(cfg.validate(), "user_agent 不能为空");
    }

    #[test]
    fn test_download_config_validate_empty_authorized_dirs() {
        let mut cfg = valid_download_config();
        cfg.authorized_dirs = vec![];
        assert_config_error(cfg.validate(), "authorized_dirs 不能为空");
    }

    #[test]
    fn test_download_config_validate_valid() {
        let cfg = valid_download_config();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_connection_config_validate_max_connections_per_host_zero() {
        let mut cfg = ConnectionConfig::default();
        cfg.max_connections_per_host = 0;
        assert_config_error(cfg.validate(), "max_connections_per_host 必须 >= 1");
    }

    #[test]
    fn test_connection_config_validate_max_connections_per_host_over_limit() {
        let mut cfg = ConnectionConfig::default();
        cfg.max_connections_per_host = MAX_CONNECTIONS_PER_HOST_LIMIT + 1;
        assert_config_error(cfg.validate(), "max_connections_per_host 不能超过");
    }

    #[test]
    fn test_connection_config_validate_max_global_connections_zero() {
        let mut cfg = ConnectionConfig::default();
        cfg.max_global_connections = 0;
        assert_config_error(cfg.validate(), "max_global_connections 必须 >= 1");
    }

    #[test]
    fn test_connection_config_validate_max_global_connections_over_limit() {
        let mut cfg = ConnectionConfig::default();
        cfg.max_global_connections = MAX_GLOBAL_CONNECTIONS_LIMIT + 1;
        assert_config_error(cfg.validate(), "max_global_connections 不能超过");
    }

    #[test]
    fn test_connection_config_validate_valid() {
        assert!(ConnectionConfig::default().validate().is_ok());
    }

    #[test]
    fn test_scheduler_config_validate_min_fragment_size_zero() {
        let mut cfg = SchedulerConfig::default();
        cfg.min_fragment_size = 0;
        assert_config_error(cfg.validate(), "min_fragment_size 必须 >= 1");
    }

    #[test]
    fn test_scheduler_config_validate_max_fragment_size_zero() {
        let mut cfg = SchedulerConfig::default();
        cfg.max_fragment_size = 0;
        assert_config_error(cfg.validate(), "max_fragment_size 必须 >= 1");
    }

    #[test]
    fn test_scheduler_config_validate_min_greater_than_max() {
        let mut cfg = SchedulerConfig::default();
        cfg.min_fragment_size = 1024;
        cfg.max_fragment_size = 512;
        assert_config_error(
            cfg.validate(),
            "min_fragment_size 不能大于 max_fragment_size",
        );
    }

    #[test]
    fn test_scheduler_config_validate_ewma_alpha_negative() {
        let mut cfg = SchedulerConfig::default();
        cfg.ewma_alpha = -0.1;
        assert_config_error(cfg.validate(), "ewma_alpha 必须在 0.0 ~ 1.0 之间");
    }

    #[test]
    fn test_scheduler_config_validate_ewma_alpha_over_one() {
        let mut cfg = SchedulerConfig::default();
        cfg.ewma_alpha = 1.1;
        assert_config_error(cfg.validate(), "ewma_alpha 必须在 0.0 ~ 1.0 之间");
    }

    #[test]
    fn test_scheduler_config_validate_ewma_beta_out_of_range() {
        let mut cfg = SchedulerConfig::default();
        cfg.ewma_beta = -0.01;
        assert_config_error(cfg.validate(), "ewma_beta 必须在 0.0 ~ 1.0 之间");
    }

    #[test]
    fn test_scheduler_config_validate_default_target_fragments_zero() {
        let mut cfg = SchedulerConfig::default();
        cfg.default_target_fragments = 0;
        assert_config_error(cfg.validate(), "default_target_fragments 必须 >= 1");
    }

    #[test]
    fn test_scheduler_config_validate_sampling_interval_zero() {
        let mut cfg = SchedulerConfig::default();
        cfg.sampling_interval_secs = 0;
        assert_config_error(cfg.validate(), "sampling_interval_secs 必须 >= 1");
    }

    #[test]
    fn test_scheduler_config_validate_valid() {
        assert!(SchedulerConfig::default().validate().is_ok());
    }

    #[test]
    fn test_app_config_validate_max_concurrent_tasks_zero() {
        let mut cfg = AppConfig::default();
        cfg.max_concurrent_tasks = 0;
        assert_config_error(cfg.validate(), "max_concurrent_tasks 必须 >= 1");
    }

    #[test]
    fn test_app_config_validate_max_concurrent_tasks_over_limit() {
        let mut cfg = AppConfig::default();
        cfg.max_concurrent_tasks = MAX_CONCURRENT_TASKS_LIMIT + 1;
        assert_config_error(cfg.validate(), "max_concurrent_tasks 不能超过");
    }

    #[test]
    fn test_app_config_validate_propagates_download_error() {
        let mut cfg = AppConfig::default();
        cfg.download.max_concurrent_fragments = 0;
        assert_config_error(cfg.validate(), "max_concurrent_fragments 必须 >= 1");
    }

    #[test]
    fn test_app_config_validate_propagates_connection_error() {
        let mut cfg = AppConfig::default();
        cfg.connection.max_connections_per_host = 0;
        assert_config_error(cfg.validate(), "max_connections_per_host 必须 >= 1");
    }

    #[test]
    fn test_app_config_validate_propagates_scheduler_error() {
        let mut cfg = AppConfig::default();
        cfg.scheduler.min_fragment_size = 0;
        assert_config_error(cfg.validate(), "min_fragment_size 必须 >= 1");
    }

    #[test]
    fn test_app_config_validate_valid() {
        assert!(AppConfig::default().validate().is_ok());
    }

    #[test]
    fn test_magnet_config_default() {
        let config = MagnetConfig::default();
        assert_eq!(config.metadata_timeout_secs, 120);
        assert_eq!(config.download_timeout_secs, 0);
        assert!(config.enable_dht, "DHT 应默认启用");
        assert!(config.enable_upnp, "UPnP 应默认启用");
        assert!(config.trackers.is_empty(), "默认 tracker 列表应为空");
    }

    #[test]
    fn test_magnet_config_validate_valid() {
        let config = MagnetConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_magnet_config_validate_metadata_timeout_zero() {
        let mut config = MagnetConfig::default();
        config.metadata_timeout_secs = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_magnet_config_with_trackers() {
        let mut config = MagnetConfig::default();
        config.trackers = vec![
            "udp://tracker.opentrackr.org:1337/announce".to_string(),
            "https://tracker.lilithraws.org:443/announce".to_string(),
        ];
        assert!(config.validate().is_ok());
        assert_eq!(config.trackers.len(), 2);
    }

    #[test]
    fn test_magnet_config_serialization_roundtrip() {
        let config = MagnetConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: MagnetConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.metadata_timeout_secs,
            config.metadata_timeout_secs
        );
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default, clippy::manual_range_contains)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    // 随机 DownloadConfig 字段边界:validate 拒绝非法值,接受合法值
    proptest! {
        #[test]
        fn test_download_config_validate_random_boundaries(
            max_concurrent_fragments in 0u32..MAX_CONCURRENT_FRAGMENTS_LIMIT + 10,
            max_retries in 0u32..MAX_RETRIES_LIMIT + 10,
            request_timeout_secs in 0u64..REQUEST_TIMEOUT_SECS_LIMIT + 10,
            connect_timeout_secs in 0u64..CONNECT_TIMEOUT_SECS_LIMIT + 10,
            pause_timeout_secs in 0u64..PAUSE_TIMEOUT_SECS_LIMIT + 10,
            max_full_stream_bytes in 0u64..1024u64,
            rate_limit in prop::option::of(0u64..2u64),
        ) {
            let mut cfg = DownloadConfig::default();
            cfg.download_dir = std::env::temp_dir().to_string_lossy().to_string();
            cfg.authorized_dirs = vec![cfg.download_dir.clone()];
            cfg.max_concurrent_fragments = max_concurrent_fragments;
            cfg.max_retries = max_retries;
            cfg.request_timeout_secs = request_timeout_secs;
            cfg.connect_timeout_secs = connect_timeout_secs;
            cfg.pause_timeout_secs = pause_timeout_secs;
            cfg.max_full_stream_bytes = max_full_stream_bytes;
            cfg.rate_limit_bytes_per_sec = rate_limit;

            let result = cfg.validate();

            let valid = max_concurrent_fragments >= 1
                && max_concurrent_fragments <= MAX_CONCURRENT_FRAGMENTS_LIMIT
                && max_retries <= MAX_RETRIES_LIMIT
                && request_timeout_secs >= 1
                && request_timeout_secs <= REQUEST_TIMEOUT_SECS_LIMIT
                && connect_timeout_secs >= 1
                && connect_timeout_secs <= CONNECT_TIMEOUT_SECS_LIMIT
                && pause_timeout_secs >= 1
                && pause_timeout_secs <= PAUSE_TIMEOUT_SECS_LIMIT
                && max_full_stream_bytes >= 1
                && rate_limit != Some(0);

            prop_assert_eq!(result.is_ok(), valid, "{}", format!("validate 结果与预期不一致: {result:?}"));
        }

        // 随机 ConnectionConfig 边界
        #[test]
        fn test_connection_config_validate_random_boundaries(
            max_connections_per_host in 0u32..MAX_CONNECTIONS_PER_HOST_LIMIT + 10,
            max_global_connections in 0u32..MAX_GLOBAL_CONNECTIONS_LIMIT + 10,
        ) {
            let mut cfg = ConnectionConfig::default();
            cfg.max_connections_per_host = max_connections_per_host;
            cfg.max_global_connections = max_global_connections;

            let result = cfg.validate();
            let valid = max_connections_per_host >= 1
                && max_connections_per_host <= MAX_CONNECTIONS_PER_HOST_LIMIT
                && max_global_connections >= 1
                && max_global_connections <= MAX_GLOBAL_CONNECTIONS_LIMIT;

            prop_assert_eq!(result.is_ok(), valid, "{}", format!("ConnectionConfig validate 不一致: {result:?}"));
        }

        // 随机 SchedulerConfig 边界
        #[test]
        fn test_scheduler_config_validate_random_boundaries(
            min_fragment_size in 0u64..2 * 1024 * 1024u64,
            max_fragment_size in 0u64..2 * 1024 * 1024u64,
            ewma_alpha in -0.5f64..1.5f64,
            ewma_beta in -0.5f64..1.5f64,
            default_target_fragments in 0u32..20,
            sampling_interval_secs in 0u64..120,
        ) {
            let mut cfg = SchedulerConfig::default();
            cfg.min_fragment_size = min_fragment_size;
            cfg.max_fragment_size = max_fragment_size;
            cfg.ewma_alpha = ewma_alpha;
            cfg.ewma_beta = ewma_beta;
            cfg.default_target_fragments = default_target_fragments;
            cfg.sampling_interval_secs = sampling_interval_secs;

            let result = cfg.validate();
            let valid = min_fragment_size >= 1
                && max_fragment_size >= 1
                && min_fragment_size <= max_fragment_size
                && (0.0..=1.0).contains(&ewma_alpha)
                && (0.0..=1.0).contains(&ewma_beta)
                && default_target_fragments >= 1
                && sampling_interval_secs >= 1;

            prop_assert_eq!(result.is_ok(), valid, "{}", format!("SchedulerConfig validate 不一致: {result:?}"));
        }

        // AppConfig 随机补丁:应用补丁后再 validate 不应 panic
        #[test]
        fn test_app_config_patch_and_validate_no_panic(
            max_concurrent_tasks in 0u32..MAX_CONCURRENT_TASKS_LIMIT + 10,
            download_dir in "[a-zA-Z0-9_\\-/]{0,30}",
        ) {
            let mut base = AppConfig::default();
            base.download.download_dir = std::env::temp_dir().to_string_lossy().to_string();
            base.download.authorized_dirs = vec![base.download.download_dir.clone()];

            let patch = ConfigPatch {
                max_concurrent_tasks: Some(max_concurrent_tasks),
                download: Some(DownloadPatch {
                    download_dir: Some(if download_dir.is_empty() {
                        std::env::temp_dir().to_string_lossy().to_string()
                    } else {
                        download_dir
                    }),
                    max_concurrent_fragments: None,
                    max_retries: None,
                    request_timeout_secs: None,
                    connect_timeout_secs: None,
                    verify_checksum: None,
                    pause_timeout_secs: None,
                    rate_limit_bytes_per_sec: None,
                    io_strategy: None,
                }),
                connection: None,
            };

            let patched = patch.apply_to(&base);
            // 仅验证不 panic;由于随机 dir 可能含非法字符,不强制 Ok
            let _ = patched.validate();
        }
    }
}
