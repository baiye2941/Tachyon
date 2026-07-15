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
        // Safety: params 为 128 字节零初始化数组(>= io_uring_params 结构体的 120 字节),
        // 作为 io_uring_setup 的第二个参数传入,内核只读取不写入(探测模式 entries=1)。
        // SYS_IO_URING_SETUP(425)在 x86_64/aarch64 Linux 正确。ring_fd >= 0 时立即 close
        // 不泄漏 fd;探测失败返回 false,不影响调用方。
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

/// 磁力链接读取 stall 超时上限(秒)
///
/// 24 小时,与 pause_timeout 对齐。0 表示禁用看门狗(向后兼容),
/// 但禁用后磁力死 swarm 会永久卡死且取消信号无法穿透。
pub const STALL_TIMEOUT_SECS_LIMIT: u64 = 86400;

/// 磁力链接无 peer 时智能等待上限(秒)
///
/// 死 swarm 下无 peer 时,协议层会持续轮询 peer 健康状态并等待 peer 上线,
/// 超过此上限则产出 `Err(Timeout)` 让引擎重试/失败。1 小时上限避免永久挂起,
/// 实际默认 2 分钟(default_peer_wait_timeout_secs)平衡恢复概率与用户体验。
pub const PEER_WAIT_TIMEOUT_SECS_LIMIT: u64 = 3600;

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

/// keep_alive_timeout_secs 上限(秒)
///
/// 与连接池 idle 超时上限对齐,10 分钟足以覆盖常规复用窗口,
/// 超过此值意味着连接长期空闲占用资源而失去 keep-alive 收益。
pub const KEEP_ALIVE_TIMEOUT_SECS_LIMIT: u64 = 600;

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
    /// 显式代理 URL,如 `http://127.0.0.1:7890`、`socks5://127.0.0.1:1080`。
    /// None 时 reqwest 读取系统环境变量(`HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`),
    /// 与 BT 侧 `detect_socks_proxy` 的“自动嗅探系统代理”语义对齐。
    #[serde(default)]
    pub proxy: Option<String>,
    /// work-stealing/动态拆分请求开关(配置兼容字段)。
    ///
    /// Phase0 运行时 hard-disable:`true` 仅表示 requested,DownloadTask 不会动态 split
    /// 慢分片,也不会改变初始静态 topology。字段保留用于配置/备份兼容与后续阶段恢复。
    /// 默认 `false`(缺字段反序列化亦为 `false`)。
    #[serde(default)]
    pub enable_work_stealing: bool,
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
    #[serde(default)]
    proxy: Option<String>,
    #[serde(default)]
    enable_work_stealing: bool,
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
            proxy: value.proxy,
            enable_work_stealing: value.enable_work_stealing,
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
            proxy: None,
            enable_work_stealing: false,
        }
    }
}

/// 磁力链接下载配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// 这些 tracker 会附加到每个磁力链接的 tracker 列表中,
    /// 即使磁力链接本身不包含 tracker 也能快速发现 peer。
    /// 格式：`udp://host:port/announce` 或 `http://host:port/announce`
    #[serde(default)]
    pub trackers: Vec<String>,
    /// 单次读取无数据 stall 超时(秒),默认 60(二级逃生舱)
    ///
    /// 磁力链接的 `FileStream` 读取在找不到 peer 时会永久挂起。引擎层
    /// 流读取循环现已 cancel-aware(select! 与 watch_for_interrupt 竞速),
    /// 取消信号可即时穿透;本字段作为二级保险,在取消信号未到达(如无
    /// control_rx 的路径)时为单次 `read` 间隔设置上限:两次数据到达间隔
    /// 超过此值则流产出 `Err(Timeout)`,触发引擎重试/失败。
    /// 0 表示禁用(`Duration::MAX` 零开销跳过,向后兼容)。
    #[serde(default = "default_stall_timeout_secs")]
    pub stall_timeout_secs: u64,
    /// 是否禁用 DHT 持久化(默认 false)
    ///
    /// librqbit 默认将 DHT 路由表持久化到磁盘,重启时复用以加速 bootstrap。
    /// 某些环境(如测试、沙箱)下持久化文件可能因文件锁或权限失败导致
    /// Session 创建报错,此时可设为 true 禁用持久化(纯内存 DHT)。
    #[serde(default)]
    pub disable_dht_persistence: bool,
    /// 无 peer 时智能等待上限(秒),默认 300(5 分钟)
    ///
    /// 死 swarm 下(磁力链接无活跃 peer)协议层会持续轮询 peer 健康状态
    /// 并等待 peer 上线,超过此上限则产出 `Err(Timeout)` 让引擎重试/失败。
    /// 0 表示禁用(回退到纯 stall_timeout 行为,向后兼容)。
    #[serde(default = "default_peer_wait_timeout_secs")]
    pub peer_wait_timeout_secs: u64,
    /// SOCKS5 代理 URL,用于让 BT tracker 和 peer 连接走代理
    ///
    /// 格式:`socks5://[username:password@]host:port`。None 表示禁用
    /// (回退自动检测系统代理)。国内访问国外 BT 资源时必需:HTTP_PROXY
    /// 环境变量只代理 HTTP tracker,UDP tracker/DHT/peer 直连会被墙。
    /// 配置后 HTTP tracker(reqwest)和 peer TCP(StreamConnector)走 socks5;
    /// UDP tracker/DHT 仍直连(socks5 不代理 UDP)。
    ///
    /// # 安全警告
    /// 若 URL 含 `user:pass@` 凭据,会**明文落盘**到 config.json
    /// (librqbit 的 `socks_proxy_url` 仅接受完整 URL 字符串,不支持环境变量传凭据,
    /// 故无法像 `DownloadConfig.proxy` 那样强制凭据走环境变量)。
    /// 如需凭据,建议用系统级代理或独立凭据管理,避免在共享环境落盘。
    #[serde(default)]
    pub socks_proxy_url: Option<String>,
    /// peer 连接超时(秒),默认 8(快于 librqbit 默认 10s 淘汰死 peer)
    #[serde(default = "default_peer_connect_timeout_secs")]
    pub peer_connect_timeout_secs: u64,
    /// peer 读写超时(秒),默认 10(与 librqbit 默认一致)
    #[serde(default = "default_peer_read_write_timeout_secs")]
    pub peer_read_write_timeout_secs: u64,
    /// 强制 tracker 重新 announce 间隔(秒),默认 120
    ///
    /// librqbit 默认遵循 tracker 返回的 interval(通常 30min-2h),
    /// 冷启动后 peer 池更新慢。强制较短间隔可更频繁发现新 peer。
    /// 0 表示禁用(遵循 tracker 默认 interval)。
    #[serde(default = "default_force_tracker_interval_secs")]
    pub force_tracker_interval_secs: u64,
    /// 延迟写入缓冲上限(MB),默认 16(慢盘优化)
    ///
    /// librqbit 攒到指定 MB 后批量落盘,减少 peer 读取循环的 I/O 等待。
    /// 0 表示禁用(同步写入)。
    #[serde(default = "default_defer_writes_up_to_mb")]
    pub defer_writes_up_to_mb: u64,
    /// SOCKS5 启用时是否禁用 DHT(默认 true)
    ///
    /// DHT 走 UDP 直连,SOCKS5 不代理 UDP,国内墙下 DHT 不可达。
    /// 禁用 DHT 避免无谓的 UDP 超时等待。
    #[serde(default = "default_true")]
    pub disable_dht_when_socks: bool,
    /// 预置 peer 地址列表(供 AddTorrentOptions.initial_peers)
    ///
    /// 格式:`host:port`。从磁力链接 `&pe=` 参数解析 + 用户手动配置合并。
    #[serde(default)]
    pub peer_addrs: Vec<String>,
}

fn default_metadata_timeout_secs() -> u64 {
    120
}

/// stall 超时默认值(秒)
///
/// 60s 给足 BT 的 DHT bootstrap + tracker 查询 + peer 握手时间(通常 20-40s),
/// 死 swarm 在 60s 内失败触发重试,避免 32 worker 永久挂起。
/// HTTP 不经此路径,不受影响。
fn default_stall_timeout_secs() -> u64 {
    60
}

/// 无 peer 时智能等待默认值(秒)
///
/// P1-T6: 从 300s(5 分钟)降到 120s(2 分钟)。
/// 国内死 swarm(tracker 全墙、DHT 关闭、PEX 无 peer)场景下,5 分钟等待
/// 体验差;120s 仍给 tracker 重试(默认 force_tracker_interval=120s)一轮机会,
/// 失败后让引擎重试/P2SP 回退 HTTP 源,而非长时间空等。
fn default_peer_wait_timeout_secs() -> u64 {
    120
}

/// peer 连接超时默认值(秒)
///
/// 8s 快于 librqbit 默认 10s,在代理网络/跨地域 swarm 中更快淘汰死 peer,
/// 腾出 128 个 peer 槽位给有效 peer。
fn default_peer_connect_timeout_secs() -> u64 {
    8
}

/// peer 读写超时默认值(秒)
///
/// 10s 与 librqbit 默认一致,平衡等待与淘汰。
fn default_peer_read_write_timeout_secs() -> u64 {
    10
}

/// 强制 tracker 重新 announce 间隔默认值(秒)
///
/// 120s 比 tracker 默认 interval(30min-2h)更频繁,加速死 swarm peer 刷新。
fn default_force_tracker_interval_secs() -> u64 {
    120
}

/// 延迟写入缓冲默认值(MB)
///
/// 16MB 平衡内存占用与慢盘 I/O 聚合收益。
fn default_defer_writes_up_to_mb() -> u64 {
    16
}

/// 预置公共 tracker 列表默认值
///
/// 含 UDP + HTTPS tracker。SOCKS5 下 UDP tracker 会被过滤(不可达),
/// HTTPS tracker 经代理可达。
fn default_trackers() -> Vec<String> {
    // P1-T1: 国内网络适配的 tracker 列表。
    //
    // 分层策略:
    // 1. HTTPS tracker(优先):SOCKS5 代理下可达,国内墙环境最可靠。
    //    覆盖 tamersunion/gbitt/explodie 等,首 announce 命中率高。
    // 2. UDP tracker(补充):直连不经 SOCKS5(librqbit 的 UDP tracker 走系统 UDP socket)。
    //    非 SOCKS5 场景下 peer 来源 +3-5 倍。
    //
    // SOCKS5 启用时 bt_session 过滤掉 udp:// tracker(见 bt_session.rs),
    // 保留 https:// 列表;非 SOCKS5 场景下全列表生效。
    vec![
        // --- HTTPS tracker(SOCKS5 可达,国内优先)---
        "https://tracker.tamersunion.org:443/announce".into(),
        "https://tracker.gbitt.info:443/announce".into(),
        "https://explodie.org:6969/announce".into(),
        "https://tracker.leechers-paradise.org:443/announce".into(),
        // --- UDP tracker(直连,非 SOCKS5 场景生效)---
        "udp://tracker.opentrackr.org:1337/announce".into(),
        "udp://open.demonii.com:1337/announce".into(),
        "udp://open.stealth.si:80/announce".into(),
        "udp://exodus.desync.com:6969/announce".into(),
        "udp://tracker.torrent.eu.org:451/announce".into(),
    ]
}

/// 自动检测系统 SOCKS5 代理 URL(供 BT tracker+peer 使用)
///
/// 检测顺序(取首个非空,每个变量名同时查大小写两形,POSIX 惯例):
/// 1. `ALL_PROXY` / `all_proxy` —— 若 scheme 是 `socks5`/`socks5h` 直接返回;
///    若是 `http(s)://host:port` 则提取 host:port 转 `socks5://host:port`
///    (Clash/V2Ray 混合端口假设)
/// 2. `HTTPS_PROXY` / `https_proxy` —— 同上提取 host:port 转 `socks5://`
/// 3. `HTTP_PROXY` / `http_proxy` —— 同上
///
/// 优先级:`ALL_PROXY > all_proxy > HTTPS_PROXY > https_proxy > HTTP_PROXY > http_proxy`,
/// 即大写优先于小写,同形之间保持上述变量优先级。
///
/// 返回 None 表示未检测到代理。调用方应让用户可手动覆盖此自动检测结果。
/// 注意:此检测假设 HTTP 代理端口同时支持 SOCKS5(Clash 混合端口通常如此),
/// 非混合端口代理会连接失败(librqbit 报错,不静默失败)。
pub fn detect_socks_proxy() -> Option<String> {
    // 尝试将任意代理 URL 规范化为 socks5://host:port
    //
    // `socks5h` 表示远程 DNS 解析,语义上等价于 `socks5`(librqbit 的
    // SocksProxyConfig 只认 socks5 scheme),故统一规范化为 socks5://host:port。
    fn normalize(url: &str) -> Option<String> {
        let parsed = url::Url::parse(url).ok()?;
        match parsed.scheme() {
            // socks5:librqbit 只认 socks5 scheme,直接返回原 URL(已含 host:port)。
            // 不走 host_str()/port() 提取 —— url crate 对非特殊 scheme(如 socks5)
            // 的 port 解析不稳定,直接用原串更可靠(与历史行为一致)。
            "socks5" => Some(url.to_string()),
            // socks5h:远程 DNS 变体,规范化 scheme 为 socks5。用字符串前缀替换,
            // 避免依赖 url crate 对 socks5h 的 host/port 提取。
            "socks5h" => Some(url.replacen("socks5h://", "socks5://", 1)),
            // http/https:提取 host:port 转 socks5(混合端口假设);http(s) 是特殊
            // scheme,url crate 能可靠解析 host/port。要求显式端口(与历史行为一致)。
            "http" | "https" => {
                let host = parsed.host_str()?;
                let port = parsed.port()?;
                Some(format!("socks5://{host}:{port}"))
            }
            _ => None,
        }
    }
    // 按优先级依次探测每个变量名的大小写两形(POSIX 惯例小写也要查)。
    // 顺序:大写优先于小写,保持 ALL_PROXY > HTTPS_PROXY > HTTP_PROXY 优先级。
    for var in [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ] {
        if let Some(url) = std::env::var(var).ok().filter(|s| !s.is_empty())
            && let Some(normalized) = normalize(&url)
        {
            return Some(normalized);
        }
    }
    None
}

/// 布尔默认值 true 的辅助函数（serde default 不支持直接写 true）
fn default_true() -> bool {
    true
}

/// 脱敏代理 URL 的 userinfo,保留 scheme/host/port(修复 B12-config)
///
/// `validate` 失败时错误信息若原样打印 `proxy`/`socks_proxy_url`,URL 含 `user:pass@`
/// 会明文泄露凭据。本函数剥离 userinfo 后重组 URL:`scheme://host:port`,
/// 供错误信息使用,保留 host:port 便于诊断定位。
/// 无法解析的 URL 回退为占位符(不泄露原串,因原串可能含凭据)。
pub fn redact_proxy_url(url: &str) -> String {
    let Ok(parsed) = url::Url::parse(url) else {
        return "<invalid-proxy-url>".to_string();
    };
    let scheme = parsed.scheme();
    let host = parsed.host_str().unwrap_or("");
    match parsed.port() {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    }
}

impl Default for MagnetConfig {
    fn default() -> Self {
        Self {
            metadata_timeout_secs: 120,
            download_timeout_secs: 0,
            enable_dht: true,
            enable_upnp: true,
            trackers: default_trackers(),
            stall_timeout_secs: default_stall_timeout_secs(),
            disable_dht_persistence: false,
            peer_wait_timeout_secs: default_peer_wait_timeout_secs(),
            socks_proxy_url: None,
            peer_connect_timeout_secs: default_peer_connect_timeout_secs(),
            peer_read_write_timeout_secs: default_peer_read_write_timeout_secs(),
            force_tracker_interval_secs: default_force_tracker_interval_secs(),
            defer_writes_up_to_mb: default_defer_writes_up_to_mb(),
            disable_dht_when_socks: true,
            peer_addrs: Vec::new(),
        }
    }
}

/// HuggingFace 源模式
///
/// 控制 HF 仓库浏览与下载时使用的源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum HfSourceMode {
    /// 直连 huggingface.co 官方源
    Official,
    /// 国内镜像 hf-mirror.com(默认,国内加速)
    #[default]
    Mirror,
    /// 官方 + 镜像多源竞速(浏览走镜像保证国内可达,下载时官方+镜像竞速)
    Race,
}

impl HfSourceMode {
    /// 该模式下浏览/列表(list_files)使用的 endpoint
    ///
    /// Mirror 与 Race 均走 hf-mirror.com:hf-mirror 提供完整 Hub API,国内可达;
    /// Official 走官方。Race 的下载阶段才注入官方+镜像竞速,浏览无需走官方。
    pub fn list_endpoint(self) -> &'static str {
        match self {
            HfSourceMode::Official => "https://huggingface.co",
            HfSourceMode::Mirror | HfSourceMode::Race => "https://hf-mirror.com",
        }
    }
}

/// HuggingFace Hub 配置
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubConfig {
    /// HF 源模式,默认 Mirror(国内加速)
    #[serde(default)]
    pub source_mode: HfSourceMode,
    /// HF 访问令牌(匿名访问时为 None)
    ///
    /// 由配置加载层从环境变量/文件填充(AGENTS.md:92 禁止各 crate 自行解析 env),
    /// `skip_serializing` 保证令牌不写入磁盘配置文件(避免明文落盘)。
    #[serde(default, skip_serializing)]
    pub token: Option<String>,
}

/// 剪贴板监听配置
///
/// 控制是否启用剪贴板 URL 自动检测。启用后后端轮询剪贴板,
/// 检测到可下载 URL 时向前端推送事件,前端弹 Toast 让用户确认下载。
/// 默认关闭(隐私尊重),用户需在设置中主动开启。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardConfig {
    /// 是否启用剪贴板监听,默认 false
    #[serde(default)]
    pub enable_watch: bool,
    /// 轮询间隔(毫秒),默认 1000
    #[serde(default = "default_clipboard_poll_interval_ms")]
    pub poll_interval_ms: u64,
}

fn default_clipboard_poll_interval_ms() -> u64 {
    1000
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            enable_watch: false,
            poll_interval_ms: default_clipboard_poll_interval_ms(),
        }
    }
}

impl ClipboardConfig {
    /// 校验配置值
    pub fn validate(&self) -> crate::DownloadResult<()> {
        let e = |msg: &str| crate::DownloadError::Config(msg.into());
        if self.poll_interval_ms == 0 {
            return Err(e("poll_interval_ms 必须 >= 1"));
        }
        Ok(())
    }
}

/// 系统通知配置
///
/// 控制任务进入 Completed/Failed 终态时是否向前端推送 `task-notification` 事件。
/// 实际是否显示原生通知由前端根据此开关决定(尊重用户偏好、避免未授权弹窗)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationsConfig {
    /// 是否启用任务终态系统通知,默认 true
    #[serde(default = "default_notifications_enabled")]
    pub enabled: bool,
}

fn default_notifications_enabled() -> bool {
    true
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: default_notifications_enabled(),
        }
    }
}

impl NotificationsConfig {
    /// 校验配置值(当前仅 bool,无需额外约束)
    pub fn validate(&self) -> crate::DownloadResult<()> {
        Ok(())
    }
}

impl MagnetConfig {
    /// 校验配置值
    pub fn validate(&self) -> crate::DownloadResult<()> {
        let e = |msg: &str| crate::DownloadError::Config(msg.into());
        if self.metadata_timeout_secs == 0 {
            return Err(e("metadata_timeout_secs 必须 >= 1"));
        }
        // stall_timeout_secs == 0 合法(禁用看门狗,向后兼容)
        if self.stall_timeout_secs > STALL_TIMEOUT_SECS_LIMIT {
            return Err(e(&format!(
                "stall_timeout_secs 不能超过 {STALL_TIMEOUT_SECS_LIMIT} (24h)"
            )));
        }
        // peer_wait_timeout_secs == 0 合法(禁用智能等待,回退纯 stall_timeout)
        if self.peer_wait_timeout_secs > PEER_WAIT_TIMEOUT_SECS_LIMIT {
            return Err(e(&format!(
                "peer_wait_timeout_secs 不能超过 {PEER_WAIT_TIMEOUT_SECS_LIMIT} (1h)"
            )));
        }
        // peer_connect_timeout_secs: 1-300
        if self.peer_connect_timeout_secs == 0 || self.peer_connect_timeout_secs > 300 {
            return Err(e(&format!(
                "peer_connect_timeout_secs 必须在 1-300 之间,实际: {}",
                self.peer_connect_timeout_secs
            )));
        }
        // peer_read_write_timeout_secs: 1-600
        if self.peer_read_write_timeout_secs == 0 || self.peer_read_write_timeout_secs > 600 {
            return Err(e(&format!(
                "peer_read_write_timeout_secs 必须在 1-600 之间,实际: {}",
                self.peer_read_write_timeout_secs
            )));
        }
        // force_tracker_interval_secs: 0(禁用) 或 30-3600
        if self.force_tracker_interval_secs != 0
            && (self.force_tracker_interval_secs < 30 || self.force_tracker_interval_secs > 3600)
        {
            return Err(e(&format!(
                "force_tracker_interval_secs 必须为 0(禁用)或 30-3600,实际: {}",
                self.force_tracker_interval_secs
            )));
        }
        // defer_writes_up_to_mb: 0-256
        if self.defer_writes_up_to_mb > 256 {
            return Err(e(&format!(
                "defer_writes_up_to_mb 不能超过 256,实际: {}",
                self.defer_writes_up_to_mb
            )));
        }
        // socks_proxy_url:Some 时校验 scheme 为 socks5(与 librqbit SocksProxyConfig 一致)
        // 修复 B12-config:错误信息用脱敏后的 URL,避免明文打印 user:pass 凭据。
        if let Some(ref url) = self.socks_proxy_url {
            let parsed = url::Url::parse(url).map_err(|_| {
                e(&format!(
                    "socks_proxy_url 不是合法 URL: {}",
                    redact_proxy_url(url)
                ))
            })?;
            if parsed.scheme() != "socks5" {
                return Err(e(&format!(
                    "socks_proxy_url scheme 必须是 socks5,实际: {}",
                    parsed.scheme()
                )));
            }
            if parsed.host_str().is_none() || parsed.port().is_none() {
                return Err(e(&format!(
                    "socks_proxy_url 必须包含 host 和 port: {}",
                    redact_proxy_url(url)
                )));
            }
        }
        // 校验 tracker URL 格式
        for (i, tracker) in self.trackers.iter().enumerate() {
            if tracker.trim().is_empty() {
                return Err(e(&format!("trackers[{}] 不能为空字符串", i)));
            }
            if url::Url::parse(tracker).is_err() {
                return Err(e(&format!("trackers[{}] 不是合法的 URL: {}", i, tracker)));
            }
            // 防止 CRLF 注入
            if tracker.contains('\r') || tracker.contains('\n') {
                return Err(e(&format!("trackers[{}] 不能包含换行符: {}", i, tracker)));
            }
        }
        Ok(())
    }
}

/// 连接配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
            // 默认启用 QUIC 意图:运行期声明优先使用 HTTP/3。
            // 实际是否生效取决于编译期是否启用 tachyon-protocol 的 `http3` feature
            // (及 reqwest_unstable cfg)——未启用时静默降级 HTTP/2,见 http.rs。
            enable_quic: true,
        }
    }
}

/// 调度器配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchedulerConfig {
    /// 最小分片大小(字节)
    pub min_fragment_size: u64,
    /// 最大分片大小(字节)
    pub max_fragment_size: u64,
    /// 动态并发度 re-recommend 间隔(秒)
    ///
    /// `execute_fragmented_download` 主循环的 `interval` 分支按此周期调用
    /// `scheduler.recommend()`,带宽变化时通过 `Semaphore::add_permits` 提升
    /// 并发度(只升不降)。带宽采样仍由"每分片完成"驱动(`observe_bandwidth`),
    /// 此字段控制的是"多久检查一次是否应提升并发度"。最小 2s 避免抖动。
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
        // proxy:Some 时校验 scheme 白名单 + 拒绝 userinfo(凭据须用环境变量传递)
        // 安全:proxy URL 可能含 user:pass@,明文落盘 config.json 会泄露凭据。
        // 此处拒绝 userinfo,强制用户通过 HTTP_PROXY/HTTPS_PROXY/ALL_PROXY 环境变量传凭据
        // (reqwest 原生支持读取这些环境变量,与 BT 侧 detect_socks_proxy 语义一致)。
        if let Some(ref url) = self.proxy {
            let parsed = url::Url::parse(url)
                .map_err(|_| e(&format!("proxy 不是合法 URL: {}", redact_proxy_url(url))))?;
            if !matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h") {
                return Err(e(&format!(
                    "proxy scheme 必须是 http/https/socks5/socks5h,实际: {}",
                    parsed.scheme()
                )));
            }
            if parsed.host_str().map(|h| h.is_empty()).unwrap_or(true) {
                return Err(e(&format!(
                    "proxy 必须包含 host: {}",
                    redact_proxy_url(url)
                )));
            }
            if !parsed.username().is_empty() || parsed.password().is_some() {
                return Err(e(&format!(
                    "proxy 禁止含 userinfo(user:pass@),请用 HTTP_PROXY/HTTPS_PROXY 环境变量传递凭据: {}",
                    redact_proxy_url(url)
                )));
            }
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
        if self.keep_alive_timeout_secs == 0 {
            return Err(e("keep_alive_timeout_secs 必须 >= 1"));
        }
        if self.keep_alive_timeout_secs > KEEP_ALIVE_TIMEOUT_SECS_LIMIT {
            return Err(e(&format!(
                "keep_alive_timeout_secs 不能超过 {KEEP_ALIVE_TIMEOUT_SECS_LIMIT}"
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigPatch {
    pub max_concurrent_tasks: Option<u32>,
    pub download: Option<DownloadPatch>,
    pub connection: Option<ConnectionPatch>,
    /// 磁力链接配置补丁
    #[serde(default)]
    pub magnet: Option<MagnetPatch>,
    /// 调度器配置补丁
    #[serde(default)]
    pub scheduler: Option<SchedulerPatch>,
    /// HuggingFace Hub 配置补丁
    #[serde(default)]
    pub hub: Option<HubPatch>,
    /// 剪贴板监听配置补丁
    #[serde(default)]
    pub clipboard: Option<ClipboardPatch>,
    /// 系统通知配置补丁
    #[serde(default)]
    pub notifications: Option<NotificationsPatch>,
}

/// 剪贴板监听配置补丁
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardPatch {
    pub enable_watch: Option<bool>,
    pub poll_interval_ms: Option<u64>,
}

impl ClipboardPatch {
    /// 将剪贴板补丁应用到现有 ClipboardConfig,仅更新 Some 字段
    pub fn apply_to(&self, base: &mut ClipboardConfig) {
        if let Some(v) = self.enable_watch {
            base.enable_watch = v;
        }
        if let Some(v) = self.poll_interval_ms {
            base.poll_interval_ms = v;
        }
    }
}

/// 系统通知配置补丁
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationsPatch {
    pub enabled: Option<bool>,
}

impl NotificationsPatch {
    /// 将通知补丁应用到现有 NotificationsConfig,仅更新 Some 字段
    pub fn apply_to(&self, base: &mut NotificationsConfig) {
        if let Some(v) = self.enabled {
            base.enabled = v;
        }
    }
}

/// 下载配置白名单补丁
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// 显式代理 URL,None 表示不修改(保留原值,可能继续用系统环境变量)
    pub proxy: Option<Option<String>>,
    /// work-stealing 请求开关补丁(配置兼容字段)。
    ///
    /// Phase0 运行时 hard-disable:`Some(true)` 仅写入 requested 状态,DownloadTask
    /// 不会动态 split。字段保留用于配置/备份兼容与后续阶段恢复。
    pub enable_work_stealing: Option<bool>,
}

/// 连接配置白名单补丁
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionPatch {
    pub max_connections_per_host: Option<u32>,
    pub max_global_connections: Option<u32>,
    pub keep_alive_timeout_secs: Option<u64>,
    pub connect_timeout_secs: Option<u64>,
    pub enable_http2: Option<bool>,
    pub enable_quic: Option<bool>,
}

/// 磁力链接配置白名单补丁
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagnetPatch {
    pub metadata_timeout_secs: Option<u64>,
    pub download_timeout_secs: Option<u64>,
    pub enable_dht: Option<bool>,
    pub enable_upnp: Option<bool>,
    pub trackers: Option<Vec<String>>,
    pub stall_timeout_secs: Option<u64>,
    pub disable_dht_persistence: Option<bool>,
    pub peer_wait_timeout_secs: Option<u64>,
    /// None=不修改,Some(None)=清空(禁用代理),Some(Some(url))=设值
    pub socks_proxy_url: Option<Option<String>>,
    pub peer_connect_timeout_secs: Option<u64>,
    pub peer_read_write_timeout_secs: Option<u64>,
    pub force_tracker_interval_secs: Option<u64>,
    pub defer_writes_up_to_mb: Option<u64>,
    pub disable_dht_when_socks: Option<bool>,
    pub peer_addrs: Option<Vec<String>>,
}

/// 调度器配置白名单补丁
///
/// 仅暴露 UI 可编辑字段。`sampling_interval_secs`(当前未生效)、
/// `default_target_fragments`、`high/medium_bandwidth_threshold` 为内部调参字段,
/// 不暴露给前端(与 `headers`/`authorized_dirs` 排除理由一致)。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchedulerPatch {
    pub min_fragment_size: Option<u64>,
    pub max_fragment_size: Option<u64>,
    pub ewma_alpha: Option<f64>,
}

/// HuggingFace Hub 配置白名单补丁
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubPatch {
    pub source_mode: Option<HfSourceMode>,
}

impl MagnetPatch {
    /// 将磁力链接补丁应用到现有 MagnetConfig,仅更新 Some 字段
    pub fn apply_to(&self, base: &mut MagnetConfig) {
        if let Some(v) = self.metadata_timeout_secs {
            base.metadata_timeout_secs = v;
        }
        if let Some(v) = self.download_timeout_secs {
            base.download_timeout_secs = v;
        }
        if let Some(v) = self.enable_dht {
            base.enable_dht = v;
        }
        if let Some(v) = self.enable_upnp {
            base.enable_upnp = v;
        }
        if let Some(v) = &self.trackers {
            base.trackers = v.clone();
        }
        if let Some(v) = self.stall_timeout_secs {
            base.stall_timeout_secs = v;
        }
        if let Some(v) = self.disable_dht_persistence {
            base.disable_dht_persistence = v;
        }
        if let Some(v) = self.peer_wait_timeout_secs {
            base.peer_wait_timeout_secs = v;
        }
        if let Some(ref v) = self.socks_proxy_url {
            base.socks_proxy_url = v.clone();
        }
        if let Some(v) = self.peer_connect_timeout_secs {
            base.peer_connect_timeout_secs = v;
        }
        if let Some(v) = self.peer_read_write_timeout_secs {
            base.peer_read_write_timeout_secs = v;
        }
        if let Some(v) = self.force_tracker_interval_secs {
            base.force_tracker_interval_secs = v;
        }
        if let Some(v) = self.defer_writes_up_to_mb {
            base.defer_writes_up_to_mb = v;
        }
        if let Some(v) = self.disable_dht_when_socks {
            base.disable_dht_when_socks = v;
        }
        if let Some(v) = &self.peer_addrs {
            base.peer_addrs = v.clone();
        }
    }
}

impl SchedulerPatch {
    /// 将调度器补丁应用到现有 SchedulerConfig,仅更新 Some 字段
    pub fn apply_to(&self, base: &mut SchedulerConfig) {
        if let Some(v) = self.min_fragment_size {
            base.min_fragment_size = v;
        }
        if let Some(v) = self.max_fragment_size {
            base.max_fragment_size = v;
        }
        if let Some(v) = self.ewma_alpha {
            base.ewma_alpha = v;
        }
    }
}

impl HubPatch {
    /// 将 Hub 补丁应用到现有 HubConfig,仅更新 Some 字段
    pub fn apply_to(&self, base: &mut HubConfig) {
        if let Some(v) = self.source_mode {
            base.source_mode = v;
        }
    }
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
        if let Some(patch) = &self.magnet {
            patch.apply_to(&mut result.magnet);
        }
        if let Some(patch) = &self.scheduler {
            patch.apply_to(&mut result.scheduler);
        }
        if let Some(patch) = &self.hub {
            patch.apply_to(&mut result.hub);
        }
        if let Some(patch) = &self.clipboard {
            patch.apply_to(&mut result.clipboard);
        }
        if let Some(patch) = &self.notifications {
            patch.apply_to(&mut result.notifications);
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
        if let Some(v) = &self.proxy {
            base.proxy = v.clone();
        }
        if let Some(v) = self.enable_work_stealing {
            base.enable_work_stealing = v;
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
        self.notifications.validate()?;
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
    /// HuggingFace Hub 配置
    #[serde(default)]
    pub hub: HubConfig,
    /// 剪贴板监听配置
    #[serde(default)]
    pub clipboard: ClipboardConfig,
    /// 系统通知配置
    #[serde(default)]
    pub notifications: NotificationsConfig,
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
            hub: HubConfig::default(),
            clipboard: ClipboardConfig::default(),
            notifications: NotificationsConfig::default(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    /// 环境变量测试串行化锁:防止 detect_socks_proxy 测试并发修改 env
    static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    /// 测试辅助:构造一个通过 validate 的 DownloadConfig(download_dir 指向临时目录)
    fn download_config_for_test() -> DownloadConfig {
        let mut cfg = DownloadConfig::default();
        // authorized_dirs 不能为空,用 download_dir 填充
        cfg.authorized_dirs = vec![cfg.download_dir.clone()];
        cfg
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
    fn test_notifications_config_default_enabled() {
        let config = NotificationsConfig::default();
        assert!(config.enabled);
    }

    #[test]
    fn test_notifications_patch_apply_to() {
        let mut config = NotificationsConfig::default();
        assert!(config.enabled);
        NotificationsPatch {
            enabled: Some(false),
        }
        .apply_to(&mut config);
        assert!(!config.enabled);
    }

    #[test]
    fn test_app_config_deserializes_missing_notifications_as_default() {
        let json = r#"{
            "maxConcurrentTasks": 3,
            "download": {
                "downloadDir": "/tmp",
                "maxConcurrentFragments": 8,
                "maxRetries": 3,
                "requestTimeoutSecs": 30,
                "connectTimeoutSecs": 10,
                "verifyChecksum": false,
                "userAgent": "Tachyon/1.0",
                "headers": {},
                "pauseTimeoutSecs": 300,
                "authorizedDirs": ["/tmp"]
            },
            "connection": {
                "maxConnectionsPerHost": 4,
                "maxGlobalConnections": 256,
                "keepAliveTimeoutSecs": 30,
                "connectTimeoutSecs": 10,
                "enableHttp2": true,
                "enableQuic": true
            },
            "scheduler": {
                "minFragmentSize": 1048576,
                "maxFragmentSize": 5242880,
                "samplingIntervalSecs": 5,
                "ewmaAlpha": 0.3
            },
            "magnet": {
                "metadataTimeoutSecs": 30,
                "downloadTimeoutSecs": 60,
                "enableDht": true,
                "enableUpnp": true,
                "trackers": [],
                "disableDhtPersistence": false,
                "peerWaitTimeoutSecs": 300
            },
            "hub": {
                "sourceMode": "mirror"
            },
            "clipboard": {
                "enableWatch": false,
                "pollIntervalMs": 1000
            }
        }"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();
        assert!(config.notifications.enabled);
    }

    #[test]
    fn test_connection_config_default() {
        let config = ConnectionConfig::default();
        assert_eq!(config.max_connections_per_host, 16);
        assert_eq!(config.max_global_connections, 256);
        assert_eq!(config.keep_alive_timeout_secs, 30);
        assert_eq!(config.connect_timeout_secs, 10);
        assert!(config.enable_http2);
        assert!(config.enable_quic); // 默认 true(运行期意图;编译期 http3 feature 可用时生效)
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

    /// 缺字段时 enable_work_stealing 必须反序列化为 false(配置契约)。
    #[test]
    fn test_enable_work_stealing_missing_field_deserializes_false() {
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
        assert!(!config.enable_work_stealing, "缺字段必须默认 false");
    }

    /// 显式 true 必须保留为 requested=true(Phase0 运行时 hard-disable 不改 schema 语义)。
    #[test]
    fn test_enable_work_stealing_explicit_true_deserializes() {
        let json = r#"{
            "downloadDir":"/tmp",
            "maxConcurrentFragments":8,
            "maxRetries":3,
            "requestTimeoutSecs":60,
            "verifyChecksum":true,
            "userAgent":"Test",
            "headers":{},
            "enableWorkStealing":true
        }"#;
        let config: DownloadConfig = serde_json::from_str(json).unwrap();
        assert!(
            config.enable_work_stealing,
            "显式 enableWorkStealing:true 必须反序列化为 true"
        );
    }

    /// 序列化输出必须包含 camelCase 字段 enableWorkStealing。
    #[test]
    fn test_enable_work_stealing_serializes_camel_case() {
        let mut config = DownloadConfig::default();
        config.enable_work_stealing = true;
        let json = serde_json::to_string(&config).unwrap();
        assert!(
            json.contains("\"enableWorkStealing\":true"),
            "序列化必须包含 enableWorkStealing:true,实际: {json}"
        );
    }

    /// DownloadPatch::apply_to 必须写入 enable_work_stealing requested 值。
    #[test]
    fn test_download_patch_enable_work_stealing_apply_true() {
        let mut base = DownloadConfig::default();
        assert!(
            !base.enable_work_stealing,
            "默认必须为 false,作为 apply 前置条件"
        );
        let patch = DownloadPatch {
            enable_work_stealing: Some(true),
            ..Default::default()
        };
        patch.apply_to(&mut base);
        assert!(
            base.enable_work_stealing,
            "DownloadPatch enable_work_stealing:Some(true) 必须 apply 到 base"
        );
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

    // ── proxy 安全校验测试 ──────────────────────────────────────────

    #[test]
    fn test_download_config_validate_proxy_http_ok() {
        let mut cfg = download_config_for_test();
        cfg.proxy = Some("http://127.0.0.1:7890".into());
        assert!(cfg.validate().is_ok(), "http 代理应通过校验");
    }

    #[test]
    fn test_download_config_validate_proxy_socks5_ok() {
        let mut cfg = download_config_for_test();
        cfg.proxy = Some("socks5://127.0.0.1:1080".into());
        assert!(cfg.validate().is_ok(), "socks5 代理应通过校验");
    }

    #[test]
    fn test_download_config_validate_proxy_https_ok() {
        let mut cfg = download_config_for_test();
        cfg.proxy = Some("https://proxy.example.com:443".into());
        assert!(cfg.validate().is_ok(), "https 代理应通过校验");
    }

    #[test]
    fn test_download_config_validate_proxy_socks5h_ok() {
        let mut cfg = download_config_for_test();
        cfg.proxy = Some("socks5h://127.0.0.1:1080".into());
        assert!(cfg.validate().is_ok(), "socks5h 代理应通过校验");
    }

    #[test]
    fn test_download_config_validate_proxy_rejects_ftp_scheme() {
        let mut cfg = download_config_for_test();
        cfg.proxy = Some("ftp://127.0.0.1:21".into());
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("scheme") && err.contains("ftp"),
            "应拒绝 ftp scheme,实际: {err}"
        );
    }

    #[test]
    fn test_download_config_validate_proxy_rejects_userinfo() {
        let mut cfg = download_config_for_test();
        let secret = "leak_me_pass";
        cfg.proxy = Some(format!("http://user:{secret}@127.0.0.1:7890"));
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("userinfo"),
            "应拒绝含 userinfo 的 proxy,实际: {err}"
        );
        assert!(!err.contains(secret), "错误信息不应泄露凭据,实际: {err}");
    }

    #[test]
    fn test_download_config_validate_proxy_rejects_invalid_url() {
        let mut cfg = download_config_for_test();
        let secret = "secret_pass";
        cfg.proxy = Some(format!("://{secret}@bad"));
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            !err.contains(secret),
            "非法 URL 错误信息不应泄露原串(可能含凭据),实际: {err}"
        );
    }

    #[test]
    fn test_download_config_validate_proxy_rejects_no_host() {
        let mut cfg = download_config_for_test();
        // "socks5://" 解析后 host_str=None(url crate 对无 host 的非 file scheme 返回 None)
        cfg.proxy = Some("socks5://".into());
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("host"), "应拒绝无 host 的 proxy,实际: {err}");
    }

    #[test]
    fn test_download_config_validate_proxy_none_ok() {
        let cfg = download_config_for_test();
        assert!(cfg.validate().is_ok(), "proxy=None 应通过校验");
    }

    #[test]
    fn test_download_config_validate_proxy_redacts_in_error() {
        let mut cfg = download_config_for_test();
        cfg.proxy = Some("http://user:password@127.0.0.1:7890".into());
        let err = cfg.validate().unwrap_err().to_string();
        // 错误信息中 host:port 保留(便于诊断),但 user:pass 必须剥离
        assert!(err.contains("127.0.0.1:7890"), "应保留 host:port: {err}");
        assert!(!err.contains("password"), "不应泄露凭据: {err}");
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
    fn test_connection_config_validate_keep_alive_timeout_zero() {
        let mut cfg = ConnectionConfig::default();
        cfg.keep_alive_timeout_secs = 0;
        assert_config_error(cfg.validate(), "keep_alive_timeout_secs 必须 >= 1");
    }

    #[test]
    fn test_connection_config_validate_keep_alive_timeout_over_limit() {
        let mut cfg = ConnectionConfig::default();
        cfg.keep_alive_timeout_secs = KEEP_ALIVE_TIMEOUT_SECS_LIMIT + 1;
        assert_config_error(cfg.validate(), "keep_alive_timeout_secs 不能超过");
    }

    #[test]
    fn test_connection_config_validate_connect_timeout_zero() {
        let mut cfg = ConnectionConfig::default();
        cfg.connect_timeout_secs = 0;
        assert_config_error(cfg.validate(), "connect_timeout_secs 必须 >= 1");
    }

    #[test]
    fn test_connection_config_validate_connect_timeout_over_limit() {
        let mut cfg = ConnectionConfig::default();
        cfg.connect_timeout_secs = CONNECT_TIMEOUT_SECS_LIMIT + 1;
        assert_config_error(cfg.validate(), "connect_timeout_secs 不能超过");
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
        assert!(!config.trackers.is_empty(), "默认 tracker 列表不应为空");
        assert_eq!(config.stall_timeout_secs, 60, "stall 超时默认 60 秒");
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
    fn test_magnet_config_validate_stall_timeout_zero_allowed() {
        // 0 合法:禁用看门狗(向后兼容)
        let mut config = MagnetConfig::default();
        config.stall_timeout_secs = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_magnet_config_validate_stall_timeout_over_limit() {
        let mut config = MagnetConfig::default();
        config.stall_timeout_secs = STALL_TIMEOUT_SECS_LIMIT + 1;
        let result = config.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("stall_timeout_secs")
        );
    }

    #[test]
    fn test_magnet_config_validate_stall_timeout_at_limit() {
        let mut config = MagnetConfig::default();
        config.stall_timeout_secs = STALL_TIMEOUT_SECS_LIMIT;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_magnet_config_validate_rejects_empty_tracker_url() {
        let mut config = MagnetConfig::default();
        config.trackers = vec!["".to_string()];
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("tracker"));
    }

    #[test]
    fn test_magnet_config_validate_rejects_invalid_tracker_url() {
        let mut config = MagnetConfig::default();
        config.trackers = vec!["not-a-valid-url".to_string()];
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("tracker"));
    }

    #[test]
    fn test_magnet_config_validate_accepts_valid_tracker_urls() {
        let mut config = MagnetConfig::default();
        config.trackers = vec![
            "udp://tracker.opentrackr.org:1337/announce".to_string(),
            "http://tracker.example.com:80/announce".to_string(),
            "https://tracker.example.com:443/announce".to_string(),
        ];
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_magnet_config_validate_rejects_tracker_with_crlf() {
        let mut config = MagnetConfig::default();
        config.trackers =
            vec!["udp://tracker.example.com:1337/announce\r\nX-Injected: true".to_string()];
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("tracker"));
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

    #[test]
    fn test_magnet_patch_apply_to_overwrites_some_fields() {
        let mut base = MagnetConfig::default();
        base.enable_dht = true;
        base.trackers = vec!["udp://old.example.com:1337/announce".to_string()];

        let patch = MagnetPatch {
            metadata_timeout_secs: Some(60),
            enable_dht: Some(false),
            trackers: Some(vec!["udp://new.example.com:1337/announce".to_string()]),
            ..Default::default()
        };
        patch.apply_to(&mut base);

        assert_eq!(base.metadata_timeout_secs, 60);
        assert_eq!(base.download_timeout_secs, 0); // None -> 保留原值
        assert!(!base.enable_dht);
        assert!(base.enable_upnp); // None -> 保留原值
        assert_eq!(base.trackers.len(), 1);
        assert_eq!(base.trackers[0], "udp://new.example.com:1337/announce");
    }

    #[test]
    fn test_magnet_patch_apply_to_preserves_all_on_none() {
        let mut base = MagnetConfig::default();
        base.metadata_timeout_secs = 200;
        base.enable_dht = false;
        base.trackers = vec!["udp://kept.example.com:1337/announce".to_string()];

        let patch = MagnetPatch::default();
        patch.apply_to(&mut base);

        assert_eq!(base.metadata_timeout_secs, 200);
        assert!(!base.enable_dht);
        assert_eq!(base.trackers.len(), 1);
    }

    #[test]
    fn test_config_patch_with_magnet_patch() {
        let base = AppConfig::default();
        assert!(!base.magnet.trackers.is_empty());

        let patch = ConfigPatch {
            max_concurrent_tasks: None,
            download: None,
            connection: None,
            magnet: Some(MagnetPatch {
                enable_dht: Some(false),
                trackers: Some(vec!["udp://tracker.example.com:1337/announce".to_string()]),
                ..Default::default()
            }),
            scheduler: None,
            hub: None,
            clipboard: None,
            notifications: None,
        };
        let result = patch.apply_to(&base);

        assert!(!result.magnet.enable_dht);
        assert_eq!(result.magnet.trackers.len(), 1);
        // 其余字段不变
        assert_eq!(result.max_concurrent_tasks, base.max_concurrent_tasks);
        assert_eq!(result.download.download_dir, base.download.download_dir);
    }

    #[test]
    fn test_clipboard_config_default() {
        let cfg = ClipboardConfig::default();
        assert!(!cfg.enable_watch, "默认应关闭剪贴板监听");
        assert_eq!(cfg.poll_interval_ms, 1000);
    }

    #[test]
    fn test_clipboard_patch_applies() {
        let mut cfg = ClipboardConfig::default();
        let patch = ClipboardPatch {
            enable_watch: Some(true),
            poll_interval_ms: Some(500),
        };
        patch.apply_to(&mut cfg);
        assert!(cfg.enable_watch);
        assert_eq!(cfg.poll_interval_ms, 500);
    }

    #[test]
    fn test_clipboard_patch_none_preserves() {
        let mut cfg = ClipboardConfig {
            enable_watch: true,
            poll_interval_ms: 2000,
        };
        let patch = ClipboardPatch::default();
        patch.apply_to(&mut cfg);
        assert!(cfg.enable_watch);
        assert_eq!(cfg.poll_interval_ms, 2000);
    }

    #[test]
    fn test_config_patch_with_clipboard() {
        let base = AppConfig::default();
        assert!(!base.clipboard.enable_watch);

        let patch = ConfigPatch {
            max_concurrent_tasks: None,
            download: None,
            connection: None,
            magnet: None,
            scheduler: None,
            hub: None,
            clipboard: Some(ClipboardPatch {
                enable_watch: Some(true),
                poll_interval_ms: Some(2000),
            }),
            notifications: None,
        };
        let result = patch.apply_to(&base);
        assert!(result.clipboard.enable_watch);
        assert_eq!(result.clipboard.poll_interval_ms, 2000);
    }

    #[test]
    fn test_magnet_patch_serialization_roundtrip() {
        let patch = MagnetPatch {
            metadata_timeout_secs: Some(60),
            download_timeout_secs: Some(300),
            enable_dht: Some(false),
            enable_upnp: Some(true),
            trackers: Some(vec!["udp://tracker.example.com:1337/announce".to_string()]),
            ..Default::default()
        };
        let json = serde_json::to_string(&patch).unwrap();
        let deserialized: MagnetPatch = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.metadata_timeout_secs, Some(60));
        assert_eq!(deserialized.enable_dht, Some(false));
        assert_eq!(deserialized.trackers.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_magnet_patch_deserializes_partial() {
        let json = r#"{"enableDht":false}"#;
        let patch: MagnetPatch = serde_json::from_str(json).unwrap();
        assert_eq!(patch.enable_dht, Some(false));
        assert!(patch.trackers.is_none());
    }

    #[test]
    fn test_magnet_patch_stall_timeout_applies() {
        let mut base = MagnetConfig::default();
        assert_eq!(base.stall_timeout_secs, 60);
        let patch = MagnetPatch {
            stall_timeout_secs: Some(120),
            ..Default::default()
        };
        patch.apply_to(&mut base);
        assert_eq!(base.stall_timeout_secs, 120);
    }

    #[test]
    fn test_magnet_patch_stall_timeout_none_preserves() {
        let mut base = MagnetConfig::default();
        base.stall_timeout_secs = 90;
        let patch = MagnetPatch::default();
        patch.apply_to(&mut base);
        assert_eq!(base.stall_timeout_secs, 90, "None 应保留原值");
    }

    #[test]
    fn test_magnet_patch_disable_dht_persistence_applies() {
        let mut base = MagnetConfig::default();
        assert!(!base.disable_dht_persistence, "默认应为 false");
        let patch = MagnetPatch {
            disable_dht_persistence: Some(true),
            ..Default::default()
        };
        patch.apply_to(&mut base);
        assert!(base.disable_dht_persistence, "应被 patch 设为 true");
    }

    #[test]
    fn test_magnet_patch_peer_wait_timeout_applies() {
        let mut base = MagnetConfig::default();
        assert_eq!(
            base.peer_wait_timeout_secs, 120,
            "默认 2 分钟(P1-T6 降低死 swarm 等待)"
        );
        let patch = MagnetPatch {
            peer_wait_timeout_secs: Some(120),
            ..Default::default()
        };
        patch.apply_to(&mut base);
        assert_eq!(base.peer_wait_timeout_secs, 120);
    }

    #[test]
    fn test_magnet_config_validate_peer_wait_timeout() {
        let mut cfg = MagnetConfig::default();
        // 0 合法(禁用智能等待)
        cfg.peer_wait_timeout_secs = 0;
        assert!(cfg.validate().is_ok());
        // 正常值
        cfg.peer_wait_timeout_secs = 600;
        assert!(cfg.validate().is_ok());
        // 超限
        cfg.peer_wait_timeout_secs = PEER_WAIT_TIMEOUT_SECS_LIMIT + 1;
        let err = cfg.validate();
        assert!(err.is_err());
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("peer_wait_timeout_secs"),
            "错误信息应包含字段名"
        );
    }

    #[test]
    fn test_magnet_config_default_has_trackers() {
        let config = MagnetConfig::default();
        assert!(!config.trackers.is_empty(), "默认 tracker 列表不应为空");
        assert!(
            config.trackers.iter().any(|t| t.starts_with("https://")),
            "默认 tracker 应含 HTTPS(SOCKS5 可达)"
        );
    }

    #[test]
    fn test_magnet_config_default_peer_opts() {
        let config = MagnetConfig::default();
        assert_eq!(config.peer_connect_timeout_secs, 8);
        assert_eq!(config.peer_read_write_timeout_secs, 10);
        assert_eq!(config.force_tracker_interval_secs, 120);
        assert_eq!(config.defer_writes_up_to_mb, 16);
        assert!(config.disable_dht_when_socks);
    }

    // ── serde default_* 函数测试:通过反序列化缺失字段的 JSON 触发 ─────
    // 这些函数仅在 JSON 缺少对应字段时被 serde 调用,Default::default() 不触发。
    // 补测让 config.rs 行覆盖率从 76% 提升到 ≥90%(覆盖 15+ 个 default_* 函数)。

    #[test]
    fn test_serde_default_magnet_timeout_secs() {
        let json = r#"{"trackers":[]}"#;
        let cfg: MagnetConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.metadata_timeout_secs, 120);
    }

    #[test]
    fn test_serde_default_magnet_enable_dht_and_upnp() {
        let json = r#"{"trackers":[]}"#;
        let cfg: MagnetConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.enable_dht, "default_true 应让 enable_dht 默认 true");
        assert!(cfg.enable_upnp, "default_true 应让 enable_upnp 默认 true");
    }

    #[test]
    fn test_serde_default_magnet_stall_timeout() {
        let json = r#"{"trackers":[]}"#;
        let cfg: MagnetConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.stall_timeout_secs, 60);
    }

    #[test]
    fn test_serde_default_magnet_peer_wait_timeout() {
        let json = r#"{"trackers":[]}"#;
        let cfg: MagnetConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.peer_wait_timeout_secs, 120);
    }

    #[test]
    fn test_serde_default_magnet_peer_connect_timeout() {
        let json = r#"{"trackers":[]}"#;
        let cfg: MagnetConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.peer_connect_timeout_secs, 8);
    }

    #[test]
    fn test_serde_default_magnet_peer_read_write_timeout() {
        let json = r#"{"trackers":[]}"#;
        let cfg: MagnetConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.peer_read_write_timeout_secs, 10);
    }

    #[test]
    fn test_serde_default_magnet_force_tracker_interval() {
        let json = r#"{"trackers":[]}"#;
        let cfg: MagnetConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.force_tracker_interval_secs, 120);
    }

    #[test]
    fn test_serde_default_magnet_defer_writes_up_to_mb() {
        let json = r#"{"trackers":[]}"#;
        let cfg: MagnetConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.defer_writes_up_to_mb, 16);
    }

    #[test]
    fn test_serde_default_magnet_disable_dht_when_socks() {
        let json = r#"{"trackers":[]}"#;
        let cfg: MagnetConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.disable_dht_when_socks);
    }

    #[test]
    fn test_serde_default_magnet_trackers() {
        // 不提供 trackers 字段,触发 #[serde(default)] → Vec::default()(空)
        // default_trackers() 仅在 Default::default() 中调用,见 test_default_trackers_*
        let json = r#"{}"#;
        let cfg: MagnetConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.trackers.is_empty(), "#[serde(default)] 产生空 Vec");
        // 通过 Default::default() 触发 default_trackers()
        let cfg2 = MagnetConfig::default();
        assert!(!cfg2.trackers.is_empty(), "default_trackers 应返回非空列表");
    }

    #[test]
    fn test_serde_default_download_connect_timeout() {
        let json = r#"{
            "downloadDir":"/tmp",
            "maxConcurrentFragments":4,
            "maxRetries":3,
            "requestTimeoutSecs":30,
            "verifyChecksum":true,
            "userAgent":"Test",
            "headers":{}
        }"#;
        let cfg: DownloadConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.connect_timeout_secs, 10);
    }

    #[test]
    fn test_serde_default_download_pause_timeout() {
        let json = r#"{
            "downloadDir":"/tmp",
            "maxConcurrentFragments":4,
            "maxRetries":3,
            "requestTimeoutSecs":30,
            "verifyChecksum":true,
            "userAgent":"Test",
            "headers":{}
        }"#;
        let cfg: DownloadConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.pause_timeout_secs, 300);
    }

    #[test]
    fn test_serde_default_download_max_full_stream_bytes() {
        let json = r#"{
            "downloadDir":"/tmp",
            "maxConcurrentFragments":4,
            "maxRetries":3,
            "requestTimeoutSecs":30,
            "verifyChecksum":true,
            "userAgent":"Test",
            "headers":{}
        }"#;
        let cfg: DownloadConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.max_full_stream_bytes, 64 * 1024 * 1024 * 1024);
    }

    #[test]
    fn test_serde_default_scheduler_all_fields() {
        // SchedulerConfig 的 min/max_fragment_size 和 ewma_alpha 无 #[serde(default)],
        // {} JSON 会失败。改用 Default::default() 触发所有 default_* 函数。
        let cfg = SchedulerConfig::default();
        assert_eq!(cfg.min_fragment_size, 1024 * 1024);
        assert_eq!(cfg.max_fragment_size, 64 * 1024 * 1024);
        assert_eq!(cfg.default_target_fragments, 16);
        assert_eq!(cfg.high_bandwidth_threshold, 100 * 1024 * 1024);
        assert_eq!(cfg.medium_bandwidth_threshold, 10 * 1024 * 1024);
        assert!((cfg.ewma_alpha - 0.3).abs() < 1e-9);
        assert!((cfg.ewma_beta - 0.09).abs() < 1e-9);
        assert_eq!(cfg.sampling_interval_secs, 60);
    }

    #[test]
    fn test_serde_default_scheduler_individual() {
        // serde 反序列化只触发有 #[serde(default)] 的字段
        let json = r#"{"minFragmentSize":500000,"maxFragmentSize":8000000,"ewmaAlpha":0.5}"#;
        let cfg: SchedulerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.min_fragment_size, 500_000);
        assert_eq!(cfg.max_fragment_size, 8_000_000);
        assert_eq!(cfg.ewma_alpha, 0.5);
        // 有 default 的字段应触发 default_* 函数
        assert_eq!(cfg.default_target_fragments, 16);
        assert_eq!(cfg.high_bandwidth_threshold, 100 * 1024 * 1024);
        assert_eq!(cfg.medium_bandwidth_threshold, 10 * 1024 * 1024);
        assert!((cfg.ewma_beta - 0.09).abs() < 1e-9);
        assert_eq!(cfg.sampling_interval_secs, 60);
    }

    #[test]
    fn test_serde_default_connection_fields() {
        // ConnectionConfig 无 #[serde(default)] 字段,用 Default::default() 触发
        let cfg = ConnectionConfig::default();
        assert_eq!(cfg.max_connections_per_host, 16);
        assert_eq!(cfg.max_global_connections, 256);
        assert_eq!(cfg.keep_alive_timeout_secs, 30);
        assert_eq!(cfg.connect_timeout_secs, 10);
        assert!(cfg.enable_http2);
        assert!(cfg.enable_quic);
    }

    #[test]
    fn test_serde_default_true_function_directly() {
        // 直接调用 default_true() 覆盖(serde 路径已覆盖,此处补直接调用)
        assert!(default_true());
    }

    #[test]
    fn test_default_trackers_contains_https_and_udp() {
        let trackers = default_trackers();
        assert!(trackers.iter().any(|t| t.starts_with("https://")));
        assert!(trackers.iter().any(|t| t.starts_with("udp://")));
    }

    #[test]
    fn test_default_magnet_config_full_defaults() {
        // 通过 Default impl 间接覆盖所有 default_* 函数(Default 内联调用 default_*)
        let cfg = MagnetConfig::default();
        assert_eq!(cfg.metadata_timeout_secs, default_metadata_timeout_secs());
        assert_eq!(cfg.stall_timeout_secs, default_stall_timeout_secs());
        assert_eq!(cfg.peer_wait_timeout_secs, default_peer_wait_timeout_secs());
        assert_eq!(
            cfg.peer_connect_timeout_secs,
            default_peer_connect_timeout_secs()
        );
        assert_eq!(
            cfg.peer_read_write_timeout_secs,
            default_peer_read_write_timeout_secs()
        );
        assert_eq!(
            cfg.force_tracker_interval_secs,
            default_force_tracker_interval_secs()
        );
        assert_eq!(cfg.defer_writes_up_to_mb, default_defer_writes_up_to_mb());
        assert_eq!(cfg.trackers, default_trackers());
    }

    #[test]
    fn test_default_scheduler_config_full_defaults() {
        let cfg = SchedulerConfig::default();
        assert_eq!(cfg.high_bandwidth_threshold, default_high_bw_threshold());
        assert_eq!(
            cfg.medium_bandwidth_threshold,
            default_medium_bw_threshold()
        );
        assert_eq!(cfg.default_target_fragments, default_target_fragments());
        assert!((cfg.ewma_beta - default_ewma_beta()).abs() < 1e-9);
        assert_eq!(cfg.sampling_interval_secs, default_sampling_interval_secs());
    }

    #[test]
    fn test_default_download_config_serde_defaults() {
        // DownloadConfig 的 default_connect_timeout_secs / default_pause_timeout_secs
        // 通过 Default impl 覆盖
        let cfg = DownloadConfig::default();
        assert_eq!(cfg.connect_timeout_secs, default_connect_timeout_secs());
        assert_eq!(cfg.pause_timeout_secs, default_pause_timeout_secs());
        assert_eq!(cfg.max_full_stream_bytes, default_max_full_stream_bytes());
    }

    #[test]
    fn test_serde_default_download_config_minimal_json() {
        // 最小 JSON(仅必填字段)触发所有可选字段的 default
        let json = r#"{
            "downloadDir":"/tmp",
            "maxConcurrentFragments":4,
            "maxRetries":3,
            "requestTimeoutSecs":30,
            "verifyChecksum":true,
            "userAgent":"Test",
            "headers":{}
        }"#;
        let cfg: DownloadConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.connect_timeout_secs, 10);
        assert_eq!(cfg.pause_timeout_secs, 300);
        assert_eq!(cfg.max_full_stream_bytes, 64 * 1024 * 1024 * 1024);
        assert_eq!(cfg.verify_strategy, VerifyStrategy::BestEffort);
        assert!(cfg.authorized_dirs.contains(&cfg.download_dir));
    }

    // ── 补覆盖:MagnetPatch 全字段 apply_to + HfSourceMode::list_endpoint ─────

    #[test]
    fn test_magnet_patch_apply_all_some_fields() {
        // 覆盖 MagnetPatch::apply_to 的所有 Some 分支(而非 ..Default::default())
        let mut cfg = MagnetConfig::default();
        let patch = MagnetPatch {
            metadata_timeout_secs: Some(200),
            download_timeout_secs: Some(600),
            enable_dht: Some(false),
            enable_upnp: Some(false),
            trackers: Some(vec!["https://custom.tracker:443/announce".into()]),
            stall_timeout_secs: Some(90),
            disable_dht_persistence: Some(true),
            peer_wait_timeout_secs: Some(100),
            socks_proxy_url: Some(Some("socks5://127.0.0.1:1080".into())),
            peer_connect_timeout_secs: Some(15),
            peer_read_write_timeout_secs: Some(20),
            force_tracker_interval_secs: Some(180),
            defer_writes_up_to_mb: Some(32),
            disable_dht_when_socks: Some(false),
            peer_addrs: Some(vec!["1.2.3.4:6881".into()]),
        };
        patch.apply_to(&mut cfg);
        assert_eq!(cfg.metadata_timeout_secs, 200);
        assert_eq!(cfg.download_timeout_secs, 600);
        assert!(!cfg.enable_dht);
        assert!(!cfg.enable_upnp);
        assert_eq!(cfg.trackers, vec!["https://custom.tracker:443/announce"]);
        assert_eq!(cfg.stall_timeout_secs, 90);
        assert!(cfg.disable_dht_persistence);
        assert_eq!(cfg.peer_wait_timeout_secs, 100);
        assert_eq!(
            cfg.socks_proxy_url.as_deref(),
            Some("socks5://127.0.0.1:1080")
        );
        assert_eq!(cfg.peer_connect_timeout_secs, 15);
        assert_eq!(cfg.peer_read_write_timeout_secs, 20);
        assert_eq!(cfg.force_tracker_interval_secs, 180);
        assert_eq!(cfg.defer_writes_up_to_mb, 32);
        assert!(!cfg.disable_dht_when_socks);
        assert_eq!(cfg.peer_addrs, vec!["1.2.3.4:6881"]);
    }

    #[test]
    fn test_magnet_patch_socks_proxy_url_clears() {
        // 覆盖 Some(None) 清空 socks_proxy_url
        let mut cfg = MagnetConfig::default();
        cfg.socks_proxy_url = Some("socks5://old:1080".into());
        let patch = MagnetPatch {
            socks_proxy_url: Some(None),
            ..Default::default()
        };
        patch.apply_to(&mut cfg);
        assert_eq!(cfg.socks_proxy_url, None);
    }

    #[test]
    fn test_hf_source_mode_list_endpoint_all_variants() {
        assert_eq!(
            HfSourceMode::Official.list_endpoint(),
            "https://huggingface.co"
        );
        assert_eq!(
            HfSourceMode::Mirror.list_endpoint(),
            "https://hf-mirror.com"
        );
        assert_eq!(HfSourceMode::Race.list_endpoint(), "https://hf-mirror.com");
    }

    #[test]
    fn test_hf_source_mode_serde_roundtrip() {
        for mode in [
            HfSourceMode::Official,
            HfSourceMode::Mirror,
            HfSourceMode::Race,
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            let back: HfSourceMode = serde_json::from_str(&json).unwrap();
            assert_eq!(mode, back);
        }
    }

    #[test]
    fn test_hub_config_token_skip_serializing() {
        let mut cfg = HubConfig::default();
        cfg.token = Some("hf_secret_token".into());
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(
            !json.contains("hf_secret_token"),
            "token 不应被序列化(skip_serializing): {json}"
        );
        // source_mode 仍应序列化
        assert!(json.contains("sourceMode"));
    }

    #[test]
    fn test_app_config_validate_max_concurrent_tasks_bounds() {
        let mut cfg = AppConfig::default();
        cfg.max_concurrent_tasks = 0;
        assert!(cfg.validate().is_err());
        cfg.max_concurrent_tasks = MAX_CONCURRENT_TASKS_LIMIT + 1;
        assert!(cfg.validate().is_err());
        cfg.max_concurrent_tasks = 5;
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_download_config_serde_with_proxy() {
        let json = r#"{
            "downloadDir":"/tmp",
            "maxConcurrentFragments":4,
            "maxRetries":3,
            "requestTimeoutSecs":30,
            "verifyChecksum":true,
            "userAgent":"Test",
            "headers":{},
            "proxy":"http://127.0.0.1:7890"
        }"#;
        let cfg: DownloadConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.proxy.as_deref(), Some("http://127.0.0.1:7890"));
    }

    #[test]
    fn test_download_config_serde_without_proxy() {
        let json = r#"{
            "downloadDir":"/tmp",
            "maxConcurrentFragments":4,
            "maxRetries":3,
            "requestTimeoutSecs":30,
            "verifyChecksum":true,
            "userAgent":"Test",
            "headers":{}
        }"#;
        let cfg: DownloadConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.proxy, None);
    }

    #[test]
    fn test_download_config_serde_io_strategy() {
        let json = r#"{
            "downloadDir":"/tmp",
            "maxConcurrentFragments":4,
            "maxRetries":3,
            "requestTimeoutSecs":30,
            "verifyChecksum":true,
            "userAgent":"Test",
            "headers":{},
            "ioStrategy":"iocp"
        }"#;
        let cfg: DownloadConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.io_strategy, IoStrategy::Iocp);
    }

    #[test]
    fn test_download_config_serde_verify_strategy_require() {
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
        let cfg: DownloadConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.verify_strategy, VerifyStrategy::Require);
    }

    // ── MagnetConfig::validate 全分支覆盖 ───────────────────────────

    #[test]
    fn test_magnet_validate_stall_timeout_over_limit() {
        let mut cfg = MagnetConfig::default();
        cfg.stall_timeout_secs = STALL_TIMEOUT_SECS_LIMIT + 1;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_magnet_validate_peer_wait_timeout_over_limit() {
        let mut cfg = MagnetConfig::default();
        cfg.peer_wait_timeout_secs = PEER_WAIT_TIMEOUT_SECS_LIMIT + 1;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_magnet_validate_peer_read_write_timeout_over_limit() {
        let mut cfg = MagnetConfig::default();
        cfg.peer_read_write_timeout_secs = 601;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_magnet_validate_force_tracker_interval_below_30() {
        let mut cfg = MagnetConfig::default();
        cfg.force_tracker_interval_secs = 15; // 非 0 且 < 30
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_magnet_validate_force_tracker_interval_over_3600() {
        let mut cfg = MagnetConfig::default();
        cfg.force_tracker_interval_secs = 3601;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_magnet_validate_force_tracker_interval_zero_ok() {
        let mut cfg = MagnetConfig::default();
        cfg.force_tracker_interval_secs = 0; // 0 = 禁用,合法
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_magnet_validate_defer_writes_over_limit() {
        let mut cfg = MagnetConfig::default();
        cfg.defer_writes_up_to_mb = 257;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_magnet_validate_socks_proxy_invalid_url() {
        let mut cfg = MagnetConfig::default();
        cfg.socks_proxy_url = Some("not a url".into());
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_magnet_validate_socks_proxy_wrong_scheme() {
        let mut cfg = MagnetConfig::default();
        cfg.socks_proxy_url = Some("http://127.0.0.1:1080".into());
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("socks5"));
    }

    #[test]
    fn test_magnet_validate_socks_proxy_missing_port() {
        let mut cfg = MagnetConfig::default();
        cfg.socks_proxy_url = Some("socks5://127.0.0.1".into()); // 无端口
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_magnet_validate_tracker_empty_string() {
        let mut cfg = MagnetConfig::default();
        cfg.trackers = vec!["  ".into()];
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_magnet_validate_tracker_invalid_url() {
        let mut cfg = MagnetConfig::default();
        cfg.trackers = vec!["not-a-url".into()];
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_magnet_validate_tracker_crlf_injection() {
        let mut cfg = MagnetConfig::default();
        cfg.trackers = vec!["https://valid.com:443/announce\r\nX-Inject: evil".into()];
        assert!(cfg.validate().is_err());
    }

    // ── ConnectionConfig::validate 全分支覆盖 ───────────────────────

    #[test]
    fn test_connection_validate_max_per_host_zero() {
        let mut cfg = ConnectionConfig::default();
        cfg.max_connections_per_host = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_connection_validate_max_per_host_over_limit() {
        let mut cfg = ConnectionConfig::default();
        cfg.max_connections_per_host = MAX_CONNECTIONS_PER_HOST_LIMIT + 1;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_connection_validate_max_global_zero() {
        let mut cfg = ConnectionConfig::default();
        cfg.max_global_connections = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_connection_validate_keep_alive_zero() {
        let mut cfg = ConnectionConfig::default();
        cfg.keep_alive_timeout_secs = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_connection_validate_connect_timeout_zero() {
        let mut cfg = ConnectionConfig::default();
        cfg.connect_timeout_secs = 0;
        assert!(cfg.validate().is_err());
    }

    // ── SchedulerConfig::validate 全分支覆盖 ────────────────────────

    #[test]
    fn test_scheduler_validate_min_fragment_zero() {
        let mut cfg = SchedulerConfig::default();
        cfg.min_fragment_size = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_scheduler_validate_max_fragment_zero() {
        let mut cfg = SchedulerConfig::default();
        cfg.max_fragment_size = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_scheduler_validate_max_lt_min() {
        let mut cfg = SchedulerConfig::default();
        cfg.min_fragment_size = 10_000_000;
        cfg.max_fragment_size = 5_000_000; // < min
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_scheduler_validate_ewma_alpha_out_of_range() {
        let mut cfg = SchedulerConfig::default();
        cfg.ewma_alpha = 1.5;
        assert!(cfg.validate().is_err());
        cfg.ewma_alpha = -0.1;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_scheduler_validate_ewma_beta_out_of_range() {
        let mut cfg = SchedulerConfig::default();
        cfg.ewma_beta = 1.5;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_scheduler_validate_default_target_fragments_zero() {
        let mut cfg = SchedulerConfig::default();
        cfg.default_target_fragments = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_scheduler_validate_sampling_interval_zero() {
        let mut cfg = SchedulerConfig::default();
        cfg.sampling_interval_secs = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_connection_validate_keep_alive_over_limit() {
        let mut cfg = ConnectionConfig::default();
        cfg.keep_alive_timeout_secs = KEEP_ALIVE_TIMEOUT_SECS_LIMIT + 1;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_connection_validate_connect_timeout_over_limit() {
        let mut cfg = ConnectionConfig::default();
        cfg.connect_timeout_secs = CONNECT_TIMEOUT_SECS_LIMIT + 1;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_connection_validate_max_global_over_limit() {
        let mut cfg = ConnectionConfig::default();
        cfg.max_global_connections = MAX_GLOBAL_CONNECTIONS_LIMIT + 1;
        assert!(cfg.validate().is_err());
    }

    // ── AppConfig::validate 补充分支 ────────────────────────────────

    #[test]
    fn test_app_config_validate_all_sub_configs_called() {
        // 确保 AppConfig::validate 调用所有子配置的 validate
        let cfg = AppConfig::default();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_magnet_config_validate_peer_connect_timeout_bounds() {
        let mut config = MagnetConfig::default();
        config.peer_connect_timeout_secs = 0;
        assert!(config.validate().is_err());
        config.peer_connect_timeout_secs = 301;
        assert!(config.validate().is_err());
        config.peer_connect_timeout_secs = 8;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_magnet_config_validate_force_tracker_interval_bounds() {
        let mut config = MagnetConfig::default();
        config.force_tracker_interval_secs = 0; // 0 合法(禁用)
        assert!(config.validate().is_ok());
        config.force_tracker_interval_secs = 29; // < 30 非法
        assert!(config.validate().is_err());
        config.force_tracker_interval_secs = 120;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_magnet_config_validate_defer_writes_up_to_mb_bounds() {
        let mut config = MagnetConfig::default();
        config.defer_writes_up_to_mb = 257;
        assert!(config.validate().is_err());
        config.defer_writes_up_to_mb = 0; // 0 合法(禁用)
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_magnet_patch_socks_proxy_url_applies() {
        let mut base = MagnetConfig::default();
        assert!(base.socks_proxy_url.is_none(), "默认 None");
        // Some(Some(url)) = 设值
        let patch = MagnetPatch {
            socks_proxy_url: Some(Some("socks5://127.0.0.1:7897".into())),
            ..Default::default()
        };
        patch.apply_to(&mut base);
        assert_eq!(
            base.socks_proxy_url.as_deref(),
            Some("socks5://127.0.0.1:7897")
        );
        // Some(None) = 清空
        let clear_patch = MagnetPatch {
            socks_proxy_url: Some(None),
            ..Default::default()
        };
        clear_patch.apply_to(&mut base);
        assert!(base.socks_proxy_url.is_none(), "Some(None) 应清空");
    }

    #[test]
    fn test_magnet_patch_peer_connect_timeout_applies() {
        let mut config = MagnetConfig::default();
        let original = config.peer_connect_timeout_secs;
        let patch = MagnetPatch {
            peer_connect_timeout_secs: Some(15),
            ..Default::default()
        };
        patch.apply_to(&mut config);
        assert_eq!(config.peer_connect_timeout_secs, 15);
        assert_ne!(config.peer_connect_timeout_secs, original);
    }

    #[test]
    fn test_magnet_patch_force_tracker_interval_applies() {
        let mut config = MagnetConfig::default();
        let patch = MagnetPatch {
            force_tracker_interval_secs: Some(300),
            ..Default::default()
        };
        patch.apply_to(&mut config);
        assert_eq!(config.force_tracker_interval_secs, 300);
    }

    #[test]
    fn test_magnet_patch_defer_writes_up_to_applies() {
        let mut config = MagnetConfig::default();
        let patch = MagnetPatch {
            defer_writes_up_to_mb: Some(32),
            ..Default::default()
        };
        patch.apply_to(&mut config);
        assert_eq!(config.defer_writes_up_to_mb, 32);
    }

    #[test]
    fn test_magnet_patch_disable_dht_when_socks_applies() {
        let mut config = MagnetConfig::default();
        let patch = MagnetPatch {
            disable_dht_when_socks: Some(false),
            ..Default::default()
        };
        patch.apply_to(&mut config);
        assert!(!config.disable_dht_when_socks);
    }

    #[test]
    fn test_magnet_patch_peer_addrs_applies() {
        let mut config = MagnetConfig::default();
        let patch = MagnetPatch {
            peer_addrs: Some(vec!["1.2.3.4:6881".into()]),
            ..Default::default()
        };
        patch.apply_to(&mut config);
        assert_eq!(config.peer_addrs, vec!["1.2.3.4:6881"]);
    }

    // ── DownloadPatch::apply_to 测试 ────────────────────────────────

    #[test]
    fn test_download_patch_overwrites_some_fields() {
        let mut cfg = DownloadConfig::default();
        cfg.max_concurrent_fragments = 8;
        cfg.max_retries = 5;
        cfg.download_dir = "/original".into();

        let patch = DownloadPatch {
            download_dir: Some("/new".into()),
            max_concurrent_fragments: Some(16),
            max_retries: None, // None = 保留原值
            request_timeout_secs: None,
            connect_timeout_secs: Some(60),
            verify_checksum: None,
            pause_timeout_secs: None,
            rate_limit_bytes_per_sec: None,
            io_strategy: None,
            proxy: Some(Some("http://127.0.0.1:7890".into())),
            enable_work_stealing: None,
        };
        patch.apply_to(&mut cfg);

        assert_eq!(cfg.download_dir, "/new");
        assert_eq!(cfg.max_concurrent_fragments, 16);
        assert_eq!(cfg.max_retries, 5, "None 字段应保留原值");
        assert_eq!(cfg.connect_timeout_secs, 60);
        assert_eq!(
            cfg.proxy.as_deref(),
            Some("http://127.0.0.1:7890"),
            "proxy Some(Some) 应覆盖"
        );
    }

    #[test]
    fn test_download_patch_preserves_all_on_none() {
        let mut cfg = DownloadConfig::default();
        let original = cfg.clone();
        let patch = DownloadPatch {
            download_dir: None,
            max_concurrent_fragments: None,
            max_retries: None,
            request_timeout_secs: None,
            connect_timeout_secs: None,
            verify_checksum: None,
            pause_timeout_secs: None,
            rate_limit_bytes_per_sec: None,
            io_strategy: None,
            proxy: None,
            enable_work_stealing: None,
        };
        patch.apply_to(&mut cfg);
        assert_eq!(cfg.download_dir, original.download_dir);
        assert_eq!(
            cfg.max_concurrent_fragments,
            original.max_concurrent_fragments
        );
        assert_eq!(cfg.proxy, original.proxy);
    }

    #[test]
    fn test_download_patch_proxy_clears_existing() {
        let mut cfg = DownloadConfig::default();
        cfg.proxy = Some("http://old:7890".into());
        let patch = DownloadPatch {
            proxy: Some(None), // Some(None) = 清空 proxy
            ..Default::default()
        };
        patch.apply_to(&mut cfg);
        assert_eq!(cfg.proxy, None, "Some(None) 应清空 proxy");
    }

    #[test]
    fn test_download_patch_rate_limit_applies() {
        let mut cfg = DownloadConfig::default();
        cfg.rate_limit_bytes_per_sec = Some(1_000_000);
        let patch = DownloadPatch {
            rate_limit_bytes_per_sec: Some(Some(2_000_000)),
            ..Default::default()
        };
        patch.apply_to(&mut cfg);
        assert_eq!(cfg.rate_limit_bytes_per_sec, Some(2_000_000));
    }

    #[test]
    fn test_download_patch_rate_limit_clears() {
        let mut cfg = DownloadConfig::default();
        cfg.rate_limit_bytes_per_sec = Some(1_000_000);
        let patch = DownloadPatch {
            rate_limit_bytes_per_sec: Some(None), // 清空限速
            ..Default::default()
        };
        patch.apply_to(&mut cfg);
        assert_eq!(cfg.rate_limit_bytes_per_sec, None);
    }

    // ── ConnectionPatch::apply_to 测试 ─────────────────────────────

    #[test]
    fn test_connection_patch_overwrites_some_fields() {
        let mut cfg = ConnectionConfig::default();
        let original_keep_alive = cfg.keep_alive_timeout_secs;
        let patch = ConnectionPatch {
            max_connections_per_host: Some(32),
            max_global_connections: None,
            keep_alive_timeout_secs: None,
            connect_timeout_secs: Some(15),
            enable_http2: Some(false),
            enable_quic: None,
        };
        patch.apply_to(&mut cfg);
        assert_eq!(cfg.max_connections_per_host, 32);
        assert_eq!(
            cfg.max_global_connections,
            ConnectionConfig::default().max_global_connections
        );
        assert_eq!(
            cfg.keep_alive_timeout_secs, original_keep_alive,
            "None 保留原值"
        );
        assert_eq!(cfg.connect_timeout_secs, 15);
        assert!(!cfg.enable_http2);
    }

    #[test]
    fn test_connection_patch_preserves_all_on_none() {
        let mut cfg = ConnectionConfig::default();
        let original = cfg.clone();
        let patch = ConnectionPatch {
            max_connections_per_host: None,
            max_global_connections: None,
            keep_alive_timeout_secs: None,
            connect_timeout_secs: None,
            enable_http2: None,
            enable_quic: None,
        };
        patch.apply_to(&mut cfg);
        assert_eq!(cfg, original);
    }

    // ── SchedulerPatch::apply_to 测试 ──────────────────────────────

    #[test]
    fn test_scheduler_patch_overwrites_some_fields() {
        let mut cfg = SchedulerConfig::default();
        let original_alpha = cfg.ewma_alpha;
        let patch = SchedulerPatch {
            min_fragment_size: Some(2_000_000),
            max_fragment_size: Some(128_000_000),
            ewma_alpha: None,
        };
        patch.apply_to(&mut cfg);
        assert_eq!(cfg.min_fragment_size, 2_000_000);
        assert_eq!(cfg.max_fragment_size, 128_000_000);
        assert_eq!(cfg.ewma_alpha, original_alpha, "None 保留原值");
    }

    #[test]
    fn test_scheduler_patch_ewma_alpha_overwrites() {
        let mut cfg = SchedulerConfig::default();
        let patch = SchedulerPatch {
            ewma_alpha: Some(0.5),
            ..Default::default()
        };
        patch.apply_to(&mut cfg);
        assert_eq!(cfg.ewma_alpha, 0.5);
    }

    #[test]
    fn test_connection_patch_all_some_fields() {
        // 覆盖 ConnectionPatch::apply_to 的所有 Some 分支
        let mut cfg = ConnectionConfig::default();
        let patch = ConnectionPatch {
            max_connections_per_host: Some(32),
            max_global_connections: Some(512),
            keep_alive_timeout_secs: Some(60),
            connect_timeout_secs: Some(15),
            enable_http2: Some(false),
            enable_quic: Some(false),
        };
        patch.apply_to(&mut cfg);
        assert_eq!(cfg.max_connections_per_host, 32);
        assert_eq!(cfg.max_global_connections, 512);
        assert_eq!(cfg.keep_alive_timeout_secs, 60);
        assert_eq!(cfg.connect_timeout_secs, 15);
        assert!(!cfg.enable_http2);
        assert!(!cfg.enable_quic);
    }

    #[test]
    fn test_download_patch_all_some_fields() {
        // 覆盖 DownloadPatch::apply_to 的所有 Some 分支
        let mut cfg = DownloadConfig::default();
        let patch = DownloadPatch {
            download_dir: Some("/patched".into()),
            max_concurrent_fragments: Some(32),
            max_retries: Some(10),
            request_timeout_secs: Some(120),
            connect_timeout_secs: Some(30),
            verify_checksum: Some(false),
            pause_timeout_secs: Some(600),
            rate_limit_bytes_per_sec: Some(Some(1_000_000)),
            io_strategy: Some(IoStrategy::Standard),
            proxy: Some(Some("http://127.0.0.1:7890".into())),
            enable_work_stealing: None,
        };
        patch.apply_to(&mut cfg);
        assert_eq!(cfg.download_dir, "/patched");
        assert_eq!(cfg.max_concurrent_fragments, 32);
        assert_eq!(cfg.max_retries, 10);
        assert_eq!(cfg.request_timeout_secs, 120);
        assert_eq!(cfg.connect_timeout_secs, 30);
        assert!(!cfg.verify_checksum);
        assert_eq!(cfg.pause_timeout_secs, 600);
        assert_eq!(cfg.rate_limit_bytes_per_sec, Some(1_000_000));
        assert_eq!(cfg.io_strategy, IoStrategy::Standard);
        assert_eq!(cfg.proxy.as_deref(), Some("http://127.0.0.1:7890"));
    }

    #[test]
    fn test_scheduler_patch_preserves_all_on_none() {
        let mut cfg = SchedulerConfig::default();
        let original = cfg.clone();
        let patch = SchedulerPatch {
            min_fragment_size: None,
            max_fragment_size: None,
            ewma_alpha: None,
        };
        patch.apply_to(&mut cfg);
        assert_eq!(cfg, original);
    }

    // ── HubPatch::apply_to 测试 ────────────────────────────────────

    #[test]
    fn test_hub_patch_overwrites_source_mode() {
        let mut cfg = HubConfig::default();
        let original = cfg.source_mode;
        // 选一个与 default 不同的值
        let new_mode = match original {
            HfSourceMode::Official => HfSourceMode::Mirror,
            _ => HfSourceMode::Official,
        };
        let patch = HubPatch {
            source_mode: Some(new_mode),
        };
        patch.apply_to(&mut cfg);
        assert_eq!(cfg.source_mode, new_mode);
        assert_ne!(cfg.source_mode, original);
    }

    #[test]
    fn test_hub_patch_preserves_on_none() {
        let mut cfg = HubConfig::default();
        let original = cfg.source_mode;
        let patch = HubPatch { source_mode: None };
        patch.apply_to(&mut cfg);
        assert_eq!(cfg.source_mode, original);
    }

    // ── ConfigPatch::apply_to 测试 ─────────────────────────────────

    #[test]
    fn test_config_patch_applies_max_concurrent_tasks() {
        let base = AppConfig::default();
        let patch = ConfigPatch {
            max_concurrent_tasks: Some(10),
            download: None,
            connection: None,
            magnet: None,
            scheduler: None,
            hub: None,
            clipboard: None,
            notifications: None,
        };
        let result = patch.apply_to(&base);
        assert_eq!(result.max_concurrent_tasks, 10);
        assert_eq!(
            result.download.download_dir, base.download.download_dir,
            "其余保留"
        );
    }

    #[test]
    fn test_config_patch_applies_nested_download_patch() {
        let base = AppConfig::default();
        let patch = ConfigPatch {
            max_concurrent_tasks: None,
            download: Some(DownloadPatch {
                download_dir: Some("/patched".into()),
                max_concurrent_fragments: Some(32),
                ..Default::default()
            }),
            connection: None,
            magnet: None,
            scheduler: None,
            hub: None,
            clipboard: None,
            notifications: None,
        };
        let result = patch.apply_to(&base);
        assert_eq!(result.download.download_dir, "/patched");
        assert_eq!(result.download.max_concurrent_fragments, 32);
        assert_eq!(
            result.max_concurrent_tasks, base.max_concurrent_tasks,
            "未 patch 字段保留"
        );
    }

    #[test]
    fn test_config_patch_applies_all_nested_patches() {
        let base = AppConfig::default();
        let patch = ConfigPatch {
            max_concurrent_tasks: Some(8),
            download: Some(DownloadPatch {
                max_retries: Some(10),
                ..Default::default()
            }),
            connection: Some(ConnectionPatch {
                max_connections_per_host: Some(32),
                ..Default::default()
            }),
            magnet: Some(MagnetPatch {
                metadata_timeout_secs: Some(60),
                ..Default::default()
            }),
            scheduler: Some(SchedulerPatch {
                min_fragment_size: Some(2_000_000),
                ..Default::default()
            }),
            hub: Some(HubPatch {
                source_mode: Some(if base.hub.source_mode == HfSourceMode::Official {
                    HfSourceMode::Mirror
                } else {
                    HfSourceMode::Official
                }),
            }),
            clipboard: None,
            notifications: None,
        };
        let result = patch.apply_to(&base);
        assert_eq!(result.max_concurrent_tasks, 8);
        assert_eq!(result.download.max_retries, 10);
        assert_eq!(result.connection.max_connections_per_host, 32);
        assert_eq!(result.magnet.metadata_timeout_secs, 60);
        assert_eq!(result.scheduler.min_fragment_size, 2_000_000);
        assert_ne!(result.hub.source_mode, base.hub.source_mode);
    }

    #[test]
    fn test_config_patch_preserves_base_on_all_none() {
        let base = AppConfig::default();
        let patch = ConfigPatch {
            max_concurrent_tasks: None,
            download: None,
            connection: None,
            magnet: None,
            scheduler: None,
            hub: None,
            clipboard: None,
            notifications: None,
        };
        let result = patch.apply_to(&base);
        assert_eq!(result.max_concurrent_tasks, base.max_concurrent_tasks);
        assert_eq!(result.download.download_dir, base.download.download_dir);
    }

    #[test]
    fn test_config_patch_does_not_mutate_base() {
        let base = AppConfig::default();
        let original = base.clone();
        let patch = ConfigPatch {
            max_concurrent_tasks: Some(99),
            ..Default::default()
        };
        let _result = patch.apply_to(&base);
        // apply_to 返回新 AppConfig,不修改原 base
        assert_eq!(base.max_concurrent_tasks, original.max_concurrent_tasks);
    }

    #[test]
    fn test_magnet_config_validate_socks_proxy_url() {
        let mut cfg = MagnetConfig::default();
        // None 合法
        assert!(cfg.validate().is_ok());
        // 合法 socks5
        cfg.socks_proxy_url = Some("socks5://127.0.0.1:7897".into());
        assert!(cfg.validate().is_ok());
        // 带 auth
        cfg.socks_proxy_url = Some("socks5://user:pass@127.0.0.1:7897".into());
        assert!(cfg.validate().is_ok());
        // 错误 scheme
        cfg.socks_proxy_url = Some("http://127.0.0.1:7897".into());
        let err = cfg.validate();
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("socks5"));
        // 缺 port
        cfg.socks_proxy_url = Some("socks5://127.0.0.1".into());
        assert!(cfg.validate().is_err());
    }

    // ── B12-config: socks_proxy_url 错误信息脱敏测试 ──────────────────────

    /// 验证:socks_proxy_url 含凭据时,validate 失败错误信息不泄露凭据(修复 B12-config)
    ///
    /// 若 socks_proxy_url 含 user:pass,validate 失败时错误信息原样打印 {url}
    /// 会明文泄露凭据。修复后用 redact_proxy_url 剥离 userinfo,只保留 scheme/host/port。
    #[test]
    fn test_magnet_config_validate_socks_url_error_redacts_credentials() {
        let secret_user = "topsecret_user";
        let secret_pass = "s3cr3t_p4ss";

        // 错误 scheme + 含凭据:错误信息不应泄露 user/pass
        let mut cfg = MagnetConfig::default();
        cfg.socks_proxy_url = Some(format!("http://{secret_user}:{secret_pass}@127.0.0.1:7897"));
        let err = cfg.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains(secret_user),
            "错误信息不应泄露用户名,实际: {msg}"
        );
        assert!(
            !msg.contains(secret_pass),
            "错误信息不应泄露密码,实际: {msg}"
        );

        // 缺 port + 含凭据:错误信息不应泄露 user/pass
        let mut cfg = MagnetConfig::default();
        cfg.socks_proxy_url = Some(format!("socks5://{secret_user}:{secret_pass}@127.0.0.1"));
        let err = cfg.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains(secret_user),
            "错误信息不应泄露用户名,实际: {msg}"
        );
        assert!(
            !msg.contains(secret_pass),
            "错误信息不应泄露密码,实际: {msg}"
        );
        // 但应保留 host 供诊断
        assert!(
            msg.contains("127.0.0.1"),
            "错误信息应保留 host 供诊断,实际: {msg}"
        );
    }

    /// 验证:非法 URL + 含凭据字符时,错误信息不泄露原串(回退占位符)
    #[test]
    fn test_magnet_config_validate_socks_url_invalid_redacts_raw() {
        let secret = "leak_me_pass";
        // 故意构造无法解析的 URL(空 scheme)+ 含凭据样字符
        let mut cfg = MagnetConfig::default();
        cfg.socks_proxy_url = Some(format!("://{secret}@127.0.0.1:7897"));
        let err = cfg.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains(secret),
            "非法 URL 错误信息不应泄露原串(可能含凭据),实际: {msg}"
        );
    }

    /// 验证:合法 socks5 + 凭据时 validate 通过(不触发错误信息路径)
    #[test]
    fn test_magnet_config_validate_socks_url_with_credentials_is_valid() {
        let mut cfg = MagnetConfig::default();
        cfg.socks_proxy_url = Some("socks5://user:pass@127.0.0.1:7897".into());
        assert!(cfg.validate().is_ok(), "带凭据的合法 socks5 URL 应通过校验");
    }

    /// 验证:redact_proxy_url 单元:剥离 userinfo 保留 scheme/host/port
    #[test]
    fn test_redact_proxy_url_strips_userinfo() {
        // 含凭据 → 剥离
        assert_eq!(
            redact_proxy_url("socks5://user:pass@127.0.0.1:7897"),
            "socks5://127.0.0.1:7897"
        );
        // 无凭据 → 原样(已脱敏)
        assert_eq!(
            redact_proxy_url("socks5://127.0.0.1:7897"),
            "socks5://127.0.0.1:7897"
        );
        // 无端口 → 保留 scheme/host
        assert_eq!(
            redact_proxy_url("socks5://user:pass@example.com"),
            "socks5://example.com"
        );
        // 非法 URL → 占位符(不泄露原串)
        assert_eq!(redact_proxy_url("not a url"), "<invalid-proxy-url>");
        // 凭据含特殊字符也不泄露
        let redacted = redact_proxy_url("socks5://p@ss:w0rd@10.0.0.1:1080");
        assert_eq!(redacted, "socks5://10.0.0.1:1080");
        assert!(!redacted.contains("w0rd"));
        // http/https scheme 同样支持
        assert_eq!(
            redact_proxy_url("http://user:pass@proxy.example.com:8080"),
            "http://proxy.example.com:8080"
        );
    }

    #[test]
    fn test_detect_socks_proxy_from_all_proxy_socks5() {
        // ALL_PROXY 含 socks5 scheme 直接用
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        // SAFETY: 测试串行化锁保护下修改进程级环境变量,测试结束前清理全部
        // 大小写变体,避免污染后续 detect_socks_proxy 测试。
        // 注意:Windows 环境变量名大小写不敏感,remove_var("all_proxy") 会清除
        // ALL_PROXY,故先清小写再 set 大写,顺序不能颠倒。
        unsafe {
            std::env::remove_var("all_proxy");
            std::env::remove_var("http_proxy");
            std::env::remove_var("https_proxy");
            std::env::set_var("ALL_PROXY", "socks5://127.0.0.1:1080");
            std::env::remove_var("HTTP_PROXY");
            std::env::remove_var("HTTPS_PROXY");
        }
        let result = detect_socks_proxy();
        assert_eq!(result.as_deref(), Some("socks5://127.0.0.1:1080"));
        // Safety: 清理仅传编译期字符串字面量给 remove_var,无裸指针解引用;ENV_TEST_LOCK
        // 串行化锁(_guard 仍持有)保证无并发 env 改动,无数据竞争/UB 风险。
        unsafe {
            std::env::remove_var("ALL_PROXY");
        }
    }

    /// 验证:socks5h(远程 DNS)被规范化为 socks5(librqbit 只认 socks5 scheme)
    #[test]
    fn test_detect_socks_proxy_from_all_proxy_socks5h_normalized() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        // SAFETY: 同上,串行化锁保护下修改并清理环境变量;先清小写再 set 大写。
        unsafe {
            std::env::remove_var("all_proxy");
            std::env::remove_var("http_proxy");
            std::env::remove_var("https_proxy");
            std::env::set_var("ALL_PROXY", "socks5h://127.0.0.1:1080");
            std::env::remove_var("HTTP_PROXY");
            std::env::remove_var("HTTPS_PROXY");
        }
        let result = detect_socks_proxy();
        assert_eq!(result.as_deref(), Some("socks5://127.0.0.1:1080"));
        // Safety: 清理仅传编译期字符串字面量给 remove_var,无裸指针解引用;ENV_TEST_LOCK
        // 串行化锁(_guard 仍持有)保证无并发 env 改动,无数据竞争/UB 风险。
        unsafe {
            std::env::remove_var("ALL_PROXY");
        }
    }

    /// 验证(POSIX):小写 all_proxy 也应被检测到
    ///
    /// 仅 Unix:POSIX 环境变量名大小写敏感,all_proxy 与 ALL_PROXY 是不同变量。
    /// Windows 环境变量名大小写不敏感,两者折叠为同一变量,此语义不适用。
    #[cfg(unix)]
    #[test]
    fn test_detect_socks_proxy_lowercase_all_proxy() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        // SAFETY: 同上,串行化锁保护下修改并清理环境变量。
        unsafe {
            std::env::remove_var("ALL_PROXY");
            std::env::set_var("all_proxy", "socks5://127.0.0.1:1080");
            std::env::remove_var("HTTP_PROXY");
            std::env::remove_var("http_proxy");
            std::env::remove_var("HTTPS_PROXY");
            std::env::remove_var("https_proxy");
        }
        let result = detect_socks_proxy();
        assert_eq!(result.as_deref(), Some("socks5://127.0.0.1:1080"));
        // Safety: 清理仅传编译期字符串字面量给 remove_var,无裸指针解引用;ENV_TEST_LOCK
        // 串行化锁(_guard 仍持有)保证无并发 env 改动,无数据竞争/UB 风险。
        unsafe {
            std::env::remove_var("all_proxy");
        }
    }

    /// 验证(POSIX):大写 ALL_PROXY 优先于小写 all_proxy
    ///
    /// 仅 Unix:大小写敏感时才有"优先级"语义;Windows 折叠为同一变量无此概念。
    #[cfg(unix)]
    #[test]
    fn test_detect_socks_proxy_uppercase_precedence_over_lowercase() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        // SAFETY: 同上,串行化锁保护下修改并清理环境变量。
        unsafe {
            std::env::set_var("ALL_PROXY", "socks5://10.0.0.1:1080");
            std::env::set_var("all_proxy", "socks5://10.0.0.2:1080");
            std::env::remove_var("HTTP_PROXY");
            std::env::remove_var("http_proxy");
            std::env::remove_var("HTTPS_PROXY");
            std::env::remove_var("https_proxy");
        }
        let result = detect_socks_proxy();
        assert_eq!(result.as_deref(), Some("socks5://10.0.0.1:1080"));
        // Safety: 清理仅传编译期字符串字面量给 remove_var,无裸指针解引用;ENV_TEST_LOCK
        // 串行化锁(_guard 仍持有)保证无并发 env 改动,无数据竞争/UB 风险。
        unsafe {
            std::env::remove_var("ALL_PROXY");
            std::env::remove_var("all_proxy");
        }
    }

    #[test]
    fn test_detect_socks_proxy_from_http_proxy_convert() {
        // HTTP_PROXY 是 http://host:port → 转 socks5://host:port
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        // SAFETY: 同上,串行化锁保护下修改并清理环境变量;先清小写再 set 大写。
        unsafe {
            std::env::remove_var("ALL_PROXY");
            std::env::remove_var("all_proxy");
            std::env::remove_var("http_proxy");
            std::env::remove_var("https_proxy");
            std::env::set_var("HTTP_PROXY", "http://127.0.0.1:7897");
            std::env::remove_var("HTTPS_PROXY");
        }
        let result = detect_socks_proxy();
        assert_eq!(result.as_deref(), Some("socks5://127.0.0.1:7897"));
        // Safety: 清理仅传编译期字符串字面量给 remove_var,无裸指针解引用;ENV_TEST_LOCK
        // 串行化锁(_guard 仍持有)保证无并发 env 改动,无数据竞争/UB 风险。
        unsafe {
            std::env::remove_var("HTTP_PROXY");
        }
    }

    #[test]
    fn test_detect_socks_proxy_none_when_unset() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        // SAFETY: 同上,串行化锁保护下修改并清理环境变量。
        unsafe {
            std::env::remove_var("ALL_PROXY");
            std::env::remove_var("all_proxy");
            std::env::remove_var("HTTP_PROXY");
            std::env::remove_var("http_proxy");
            std::env::remove_var("HTTPS_PROXY");
            std::env::remove_var("https_proxy");
        }
        assert!(detect_socks_proxy().is_none());
    }

    /// 覆盖 `DownloadConfig::default` 中 `dirs()` 返回 None 的回退路径(L298-302)。
    ///
    /// 当 `USERPROFILE` 和 `HOME` 环境变量均不存在时,`dirs()` 返回 None,
    /// `download_dir` 回退到 `std::env::temp_dir().join("tachyon-downloads")`。
    /// 此路径在正常测试环境(总有 HOME/USERPROFILE)下不会执行,需临时清除环境变量触发。
    #[test]
    fn test_download_config_default_falls_back_to_temp_dir_when_no_home() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        // SAFETY: ENV_TEST_LOCK 串行化锁保护下临时修改进程级环境变量,
        // 测试结束前恢复原值。仅传编译期字符串字面量给 remove_var/set_var,
        // 无裸指针解引用,无数据竞争/UB 风险。
        let saved_userprofile = std::env::var_os("USERPROFILE");
        let saved_home = std::env::var_os("HOME");
        unsafe {
            std::env::remove_var("USERPROFILE");
            std::env::remove_var("HOME");
        }
        let cfg = DownloadConfig::default();
        // 恢复环境变量(无论断言结果如何,确保不污染后续测试)
        unsafe {
            if let Some(v) = saved_userprofile {
                std::env::set_var("USERPROFILE", v);
            }
            if let Some(v) = saved_home {
                std::env::set_var("HOME", v);
            }
        }
        // 回退路径应产生 temp_dir/tachyon-downloads
        assert!(
            cfg.download_dir.contains("tachyon-downloads"),
            "dirs() 返回 None 时应回退到 temp_dir/tachyon-downloads,实际: {}",
            cfg.download_dir
        );
        // authorized_dirs 应回退为 [download_dir](From 实现的 unwrap_or_else 路径)
        assert!(
            cfg.authorized_dirs.contains(&cfg.download_dir),
            "authorized_dirs 应包含 download_dir"
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
                    proxy: None,
            enable_work_stealing: None,
                }),
                connection: None,
                magnet: None,
                scheduler: None,
                hub: None,
                clipboard: None,
            notifications: None,
        };

            let patched = patch.apply_to(&base);
            // 仅验证不 panic;由于随机 dir 可能含非法字符,不强制 Ok
            let _ = patched.validate();
        }
    }
}
