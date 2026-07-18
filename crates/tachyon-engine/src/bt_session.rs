//! BitTorrent Session 单例管理
//!
//! 类似 ConnectionPool 的全局单例模式，
//! 在 Tauri setup 钩子中创建，随应用生命周期存在。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use librqbit::{PeerConnectionOptions, Session, SessionOptions};
use tachyon_core::config::MagnetConfig;
use tachyon_protocol::magnet::{HandleCache, SessionOpsGate};

/// 脱敏 SOCKS 代理 URL 的凭据,保留 scheme/host/port 供日志排查
///
/// SOCKS 代理 URL 常含 user:pass 凭据(如 `socks5://user:pass@127.0.0.1:1080`),
/// 明文打印会泄漏到日志聚合/SIEM/共享日志文件。本函数剥离 username/password,
/// 保留 scheme/host/port(对代理排查必要),解析失败时返回固定占位符。
///
/// 不复用 `tachyon_core::safety::redact_url_for_log`:该函数面向 http 下载 URL,
/// 取 `host_str()`(不含 port)并拼 basename,对 SOCKS 代理 URL 会丢失 port,
/// 而 SOCKS 代理的端口(1080/7890/...)是排查必要信息。
///
/// 输入 `socks5://user:pass@127.0.0.1:1080` -> 输出 `socks5://127.0.0.1:1080`。
fn redact_socks_proxy_for_log(proxy: &str) -> String {
    match url::Url::parse(proxy) {
        Ok(mut url) => {
            // 剥离凭据:set_username/set_password 在有 host 时返回 Ok,失败也无碍
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.to_string()
        }
        Err(_) => "<invalid proxy url>".to_string(),
    }
}

/// BitTorrent Session 单例
///
/// 封装 librqbit Session，提供全局共享的 BitTorrent 引擎实例。
/// 在 tachyon-app 的 Tauri setup 钩子中创建，通过 Arc 共享注入。
pub struct BtSession {
    inner: Arc<Session>,
    config: MagnetConfig,
    download_dir: PathBuf,
    /// 审计 A-09:构建时解析的 effective SOCKS(显式或环境检测),供覆盖报告。
    /// 不热检测环境变化;仅 Session 创建/重建时解析。
    effective_socks_proxy: Option<String>,
    /// 审计 A-09:effective SOCKS 来源
    socks_source: SocksProxySource,
    /// 跨 MagnetProtocol 实例共享的 handle 缓存。
    ///
    /// 同一 binding(download_dir + factory + preferred + url)的多次 probe/run
    /// 可共享 handle,避免重复 add_magnet_to_session。
    /// UI `probe_filename`(无 factory)与下载任务(有 TachyonStorageFactory)绑定不同,
    /// 不跨 binding 共享;probe 结束后 stop_and_remove,ops_gate 串行 delete/add。
    /// 热切换重建 BtSession 时,新实例自带空 cache,旧 handle 随旧 Session 丢弃。
    handle_cache: HandleCache,
    /// 与所有 MagnetProtocol 共享:probe cleanup 与 download add 串行
    ops_gate: SessionOpsGate,
}

impl BtSession {
    /// 创建 BitTorrent Session
    ///
    /// `download_dir` 为默认下载输出目录。
    /// 根据 MagnetConfig 配置 DHT、UPnP、全局 tracker 等选项。
    ///
    /// # DHT 持久化
    ///
    /// librqbit 默认启用 DHT 持久化（`disable_dht_persistence: false`），
    /// 会自动在默认位置存储 DHT 节点信息，重启时复用已知节点加速 bootstrap。
    pub async fn new(
        download_dir: PathBuf,
        config: MagnetConfig,
    ) -> tachyon_core::DownloadResult<Self> {
        let (opts, effective_socks_proxy, socks_source) = Self::build_session_options(&config);

        let session = Session::new_with_opts(download_dir.clone(), opts)
            .await
            .map_err(|e| {
                tachyon_core::DownloadError::Config(format!("创建 BitTorrent Session 失败: {e}"))
            })?;

        Ok(Self {
            inner: session,
            config,
            download_dir,
            effective_socks_proxy,
            socks_source,
            handle_cache: Arc::new(DashMap::new()),
            ops_gate: Arc::new(DashMap::new()),
        })
    }

    /// 根据 MagnetConfig 构造 SessionOptions + effective SOCKS(纯函数,可独立测试)
    ///
    /// 填充:peer_opts(connect/read_write 超时)、defer_writes_up_to、
    /// SOCKS5 代理 + DHT 联动、tracker 注入(SOCKS5 下过滤 UDP,追加 HTTPS)。
    ///
    /// 返回 `(SessionOptions, effective_socks, socks_source)`。
    fn build_session_options(
        config: &MagnetConfig,
    ) -> (SessionOptions, Option<String>, SocksProxySource) {
        // SOCKS5 检测:用户配置优先,否则自动检测系统代理
        let (socks_proxy, socks_source) = if let Some(ref explicit) = config.socks_proxy_url {
            (Some(explicit.clone()), SocksProxySource::Explicit)
        } else if let Some(detected) = tachyon_core::config::detect_socks_proxy() {
            tracing::info!(
                proxy = %redact_socks_proxy_for_log(&detected),
                "自动检测到系统 SOCKS5 代理(BT tracker+peer 将走代理)"
            );
            (Some(detected), SocksProxySource::Environment)
        } else {
            (None, SocksProxySource::None)
        };
        let socks_enabled = socks_proxy.is_some();

        // 审计 BT-11:high_privacy 强制禁 DHT/UPnP/公共 tracker 注入
        let high_privacy = config.high_privacy;
        if high_privacy {
            tracing::info!(
                "BT 高隐私模式启用:禁 DHT、禁 UPnP、不注入全局 tracker(metadata 阶段依赖限制见文档)"
            );
        }

        // DHT:high_privacy 强制禁用;否则 SOCKS5 下按 disable_dht_when_socks 决定
        let disable_dht = if high_privacy {
            true
        } else if socks_enabled && config.disable_dht_when_socks {
            tracing::info!("SOCKS5 启用且 disable_dht_when_socks=true,禁用 DHT(UDP 不可达)");
            true
        } else {
            !config.enable_dht
        };

        let enable_upnp = if high_privacy {
            false
        } else {
            config.enable_upnp
        };

        let mut opts = SessionOptions {
            disable_dht,
            enable_upnp_port_forwarding: enable_upnp,
            disable_dht_persistence: config.disable_dht_persistence,
            // peer 连接超时调优(快速淘汰死 peer,腾出 128 槽位)
            peer_opts: Some(PeerConnectionOptions {
                connect_timeout: Some(Duration::from_secs(config.peer_connect_timeout_secs)),
                read_write_timeout: Some(Duration::from_secs(config.peer_read_write_timeout_secs)),
                ..Default::default()
            }),
            // 延迟写入缓冲(慢盘优化,0 禁用)
            defer_writes_up_to: if config.defer_writes_up_to_mb == 0 {
                None
            } else {
                Some(config.defer_writes_up_to_mb as usize)
            },
            ..Default::default()
        };

        // SOCKS5 代理
        if let Some(ref proxy) = socks_proxy {
            opts.socks_proxy_url = Some(proxy.clone());
            tracing::info!(
                proxy = %redact_socks_proxy_for_log(proxy),
                source = ?socks_source,
                "BT SOCKS5 代理已启用"
            );
        }

        // BT-15:enable_upnp=true 时自动设 listen_port_range(默认 6881..6889),
        // 否则 librqbit 不创建 TCP listener,UPnP 静默不启动,无入站 peer。
        // high_privacy=true 时 enable_upnp=false,不触发自动设端口。
        if enable_upnp && opts.listen_port_range.is_none() {
            opts.listen_port_range = Some(6881..6889);
            tracing::info!(
                "enable_upnp=true 自动启用 BT listen_port_range=6881..6889(入站 peer + UPnP)"
            );
        }

        // tracker 注入:high_privacy 完全跳过(不注入公共/全局 tracker)
        // SOCKS5 下过滤 UDP(不可达),追加 HTTPS(经代理可达)
        if high_privacy {
            tracing::info!("高隐私模式:跳过 Session 全局 tracker 注入");
        }
        for tracker_url in &config.trackers {
            if high_privacy {
                break;
            }
            let is_udp = tracker_url.starts_with("udp://");
            if socks_enabled && is_udp {
                tracing::debug!(tracker = %tracker_url, "SOCKS5 启用,跳过 UDP tracker(不可达)");
                continue;
            }
            if let Ok(url) = url::Url::parse(tracker_url) {
                opts.trackers.insert(url);
            }
        }
        if socks_enabled && !high_privacy {
            const HTTPS_TRACKERS_FOR_PROXY: &[&str] = &[
                "https://tracker.tamersunion.org:443/announce",
                "https://tracker.gbitt.info:443/announce",
            ];
            for https_tracker in HTTPS_TRACKERS_FOR_PROXY {
                if let Ok(url) = url::Url::parse(https_tracker) {
                    opts.trackers.insert(url);
                }
            }
            tracing::info!("SOCKS5 启用,追加 HTTPS tracker(经代理可达)");
        }

        (opts, socks_proxy, socks_source)
    }

    /// FIX-16:报告 BT 各流量类别在当前配置下的代理覆盖状态(隐私可见性)。
    ///
    /// 审计 A-09:基于 Session 构建时解析的 **effective** SOCKS(显式或环境检测),
    /// 而非仅看 MagnetConfig.socks_proxy_url。环境变化不热检测——仅创建/重建时解析。
    pub fn proxy_coverage_status(&self) -> ProxyCoverageReport {
        bt_proxy_coverage_status_effective(
            &self.config,
            self.effective_socks_proxy.is_some(),
            self.socks_source,
            self.effective_socks_proxy
                .as_deref()
                .map(redact_socks_proxy_for_log),
        )
    }

    /// 审计 A-09:测试/调试 — 当前 Session 的 effective SOCKS 来源
    pub fn socks_source(&self) -> SocksProxySource {
        self.socks_source
    }

    /// 审计 A-09:测试/调试 — 脱敏 effective endpoint
    pub fn effective_socks_redacted(&self) -> Option<String> {
        self.effective_socks_proxy
            .as_deref()
            .map(redact_socks_proxy_for_log)
    }

    /// 获取内部 Session 引用
    pub fn session(&self) -> Arc<Session> {
        self.inner.clone()
    }

    /// 获取磁力链接配置
    pub fn config(&self) -> &MagnetConfig {
        &self.config
    }

    /// 获取默认下载目录
    pub fn download_dir(&self) -> &PathBuf {
        &self.download_dir
    }

    /// 获取跨实例共享的 handle 缓存(Arc 浅克隆)
    ///
    /// 所有 MagnetProtocol 传入同一 Arc,使同 binding 的 handle 跨实例共享。
    pub fn handle_cache(&self) -> HandleCache {
        Arc::clone(&self.handle_cache)
    }

    /// 跨实例共享的 session 操作锁表
    pub fn ops_gate(&self) -> SessionOpsGate {
        Arc::clone(&self.ops_gate)
    }
}

/// FIX-16:BT 某类流量相对 SOCKS 代理的覆盖状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ProxyCoverage {
    /// 未配置 SOCKS,流量直连(无代理保护)。
    Direct,
    /// 已配置 SOCKS 且应用已注入 socks_proxy_url,流量经代理。
    /// 注意:ViaProxy 表示「应用已配置」,不等于「已证实全程未泄漏」。
    ViaProxy,
    /// 流量被过滤/禁用(如 SOCKS 下 UDP tracker/DHT 不可达被关闭),不产生流量。
    Blocked,
    /// 功能关闭(如 UPnP=false),不产生流量。
    Disabled,
    /// 流量可能绕过代理(如 uTP/UPnP 基于 UDP 或局域网,SOCKS5 不代理 UDP)。
    MayBypass,
}

/// FIX-16:BT 各流量类别的代理覆盖报告(隐私可见性)。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyCoverageReport {
    /// 是否已启用 SOCKS5 代理(显式配置或构建时环境检测)。
    pub socks_enabled: bool,
    /// 对等 TCP 连接(librqbit peer TCP,经 socks_proxy_url)。
    pub peer_tcp: ProxyCoverage,
    /// HTTP(S) tracker(reqwest,经 socks_proxy_url)。
    pub http_tracker: ProxyCoverage,
    /// UDP tracker + DHT(基于 UDP,SOCKS5 不代理 UDP)。
    pub udp_tracker_dht: ProxyCoverage,
    /// uTP(基于 UDP 的传输,SOCKS5 不代理 UDP,可能绕过)。
    pub utp: ProxyCoverage,
    /// UPnP(局域网端口映射,不走 SOCKS,可能绕过)。
    pub upnp: ProxyCoverage,
    /// 审计 A-09:运行时 SOCKS 来源(none/explicit/environment)
    pub socks_source: SocksProxySource,
    /// 审计 A-09:脱敏后的 effective SOCKS endpoint(无凭据);未启用时为 None
    pub socks_endpoint_redacted: Option<String>,
}

/// 审计 A-09:Session 构建时解析的 SOCKS 代理来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SocksProxySource {
    /// 未启用 SOCKS
    None,
    /// MagnetConfig.socks_proxy_url 显式配置
    Explicit,
    /// detect_socks_proxy() 环境/系统检测
    Environment,
}

/// FIX-16:根据 MagnetConfig 计算 BT 各流量类别的代理覆盖状态(纯函数,可独立测试)。
///
/// 兼容层:仅看显式 `socks_proxy_url`(确定性测试)。生产路径请用
/// [`bt_proxy_coverage_status_effective`] 传入 Session 构建时的 effective SOCKS。
pub fn bt_proxy_coverage_status(config: &MagnetConfig) -> ProxyCoverageReport {
    let socks_enabled = config.socks_proxy_url.is_some();
    let source = if socks_enabled {
        SocksProxySource::Explicit
    } else {
        SocksProxySource::None
    };
    let endpoint = config
        .socks_proxy_url
        .as_deref()
        .map(redact_socks_proxy_for_log);
    bt_proxy_coverage_status_effective(config, socks_enabled, source, endpoint)
}

/// 审计 A-09:基于 effective SOCKS 计算覆盖报告。
///
/// `socks_enabled` 来自 Session 构建时注入值(显式或 detect_socks_proxy),
/// 可与 config.socks_proxy_url=None 但环境代理生效的情况对齐。
pub fn bt_proxy_coverage_status_effective(
    config: &MagnetConfig,
    socks_enabled: bool,
    socks_source: SocksProxySource,
    socks_endpoint_redacted: Option<String>,
) -> ProxyCoverageReport {
    if !socks_enabled {
        // 无 SOCKS:所有流量直连(UPnP 按开关区分 Direct/Disabled)
        let upnp = if config.enable_upnp {
            ProxyCoverage::Direct
        } else {
            ProxyCoverage::Disabled
        };
        return ProxyCoverageReport {
            socks_enabled: false,
            peer_tcp: ProxyCoverage::Direct,
            http_tracker: ProxyCoverage::Direct,
            udp_tracker_dht: ProxyCoverage::Direct,
            utp: ProxyCoverage::Direct,
            upnp,
            socks_source: SocksProxySource::None,
            socks_endpoint_redacted: None,
        };
    }
    // SOCKS 启用
    // peer TCP / HTTP tracker:应用已注入 socks_proxy_url -> ViaProxy
    let peer_tcp = ProxyCoverage::ViaProxy;
    let http_tracker = ProxyCoverage::ViaProxy;
    // UDP tracker + DHT:SOCKS5 不代理 UDP。disable_dht_when_socks=true 时应用禁用 DHT、
    // 过滤 UDP tracker -> Blocked;否则未禁用但 UDP 不经代理 -> MayBypass
    let udp_tracker_dht = if config.disable_dht_when_socks {
        ProxyCoverage::Blocked
    } else {
        ProxyCoverage::MayBypass
    };
    // uTP 基于 UDP,SOCKS5 不代理 UDP -> MayBypass
    let utp = ProxyCoverage::MayBypass;
    // UPnP 局域网端口映射,不走 SOCKS:关闭时 Disabled,开启时 MayBypass
    let upnp = if config.enable_upnp {
        ProxyCoverage::MayBypass
    } else {
        ProxyCoverage::Disabled
    };
    ProxyCoverageReport {
        socks_enabled: true,
        peer_tcp,
        http_tracker,
        udp_tracker_dht,
        utp,
        upnp,
        socks_source,
        socks_endpoint_redacted,
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// 构造不启用 DHT/UPnP 的最小配置(测试用,避免真实网络副作用)
    fn test_config() -> MagnetConfig {
        let mut config = MagnetConfig::default();
        config.enable_dht = false;
        config.enable_upnp = false;
        config.disable_dht_persistence = true;
        // 清空默认 tracker,测试自行注入可控行为
        config.trackers = Vec::new();
        config
    }

    #[test]
    fn test_peer_opts_filled_from_config() {
        let mut config = test_config();
        config.peer_connect_timeout_secs = 5;
        config.peer_read_write_timeout_secs = 7;
        let (opts, _effective, _source) = BtSession::build_session_options(&config);

        let peer_opts = opts.peer_opts.expect("peer_opts 应为 Some(由 config 填充)");
        assert_eq!(
            peer_opts.connect_timeout,
            Some(Duration::from_secs(5)),
            "connect_timeout 应取自 peer_connect_timeout_secs"
        );
        assert_eq!(
            peer_opts.read_write_timeout,
            Some(Duration::from_secs(7)),
            "read_write_timeout 应取自 peer_read_write_timeout_secs"
        );
    }

    #[test]
    fn test_defer_writes_filled_when_nonzero() {
        let mut config = test_config();
        config.defer_writes_up_to_mb = 32;
        let (opts, _effective, _source) = BtSession::build_session_options(&config);
        assert_eq!(
            opts.defer_writes_up_to,
            Some(32),
            "defer_writes_up_to 应为 MB 值(usize)"
        );
    }

    #[test]
    fn test_defer_writes_disabled_when_zero() {
        let mut config = test_config();
        config.defer_writes_up_to_mb = 0;
        let (opts, _effective, _source) = BtSession::build_session_options(&config);
        assert_eq!(
            opts.defer_writes_up_to, None,
            "defer_writes_up_to=0 应映射为 None(禁用)"
        );
    }

    #[test]
    fn test_no_socks_keeps_udp_trackers() {
        // 显式不设 socks_proxy_url 且无系统代理环境 → SOCKS5 关闭,
        // UDP tracker 应被保留(不过滤)
        let mut config = test_config();
        config.socks_proxy_url = None;
        config.trackers = vec![
            "udp://tracker.opentrackr.org:1337/announce".into(),
            "https://tracker.example.org:443/announce".into(),
        ];

        // 确保检测不到系统代理(清掉相关环境变量,本测试内局部清理)
        let vars = ["ALL_PROXY", "HTTPS_PROXY", "HTTP_PROXY"];
        let saved: Vec<(&str, Option<std::ffi::OsString>)> =
            vars.iter().map(|v| (*v, std::env::var_os(v))).collect();
        for v in vars {
            // SAFETY: 测试串行运行(本测试独占修改 env),且仅测试期间临时清空
            // 代理环境变量。下方 finally 风格恢复保证不泄漏到其他测试。
            unsafe {
                std::env::remove_var(v);
            }
        }

        let (opts, _effective, _source) = BtSession::build_session_options(&config);

        // 恢复环境变量
        for (v, val) in saved {
            if let Some(val) = val {
                // SAFETY: 同上,恢复原值
                unsafe {
                    std::env::set_var(v, val);
                }
            }
        }

        assert!(
            opts.socks_proxy_url.is_none(),
            "无 SOCKS5 时 socks_proxy_url 应为 None"
        );
        // enable_dht=false → disable_dht=true;此处仅断言 SOCKS5 未额外影响
        assert!(
            opts.disable_dht,
            "enable_dht=false 时 disable_dht 应为 true"
        );
        // UDP tracker 应被保留
        let tracker_schemes: HashSet<&str> = opts.trackers.iter().map(|u| u.scheme()).collect();
        assert!(
            tracker_schemes.contains("udp"),
            "无 SOCKS5 时 UDP tracker 不应被过滤,实际: {tracker_schemes:?}"
        );
        assert_eq!(
            opts.trackers.len(),
            2,
            "不应追加额外 HTTPS tracker(非 SOCKS5 模式)"
        );
    }

    #[test]
    fn test_high_privacy_disables_dht_upnp_and_trackers() {
        let mut config = MagnetConfig::default();
        config.high_privacy = true;
        config.enable_dht = true;
        config.enable_upnp = true;
        config.trackers = vec!["https://tracker.example/announce".into()];
        config.socks_proxy_url = None;
        let (opts, _effective, _source) = BtSession::build_session_options(&config);
        assert!(opts.disable_dht, "high_privacy 应禁用 DHT");
        assert!(
            !opts.enable_upnp_port_forwarding,
            "high_privacy 应禁用 UPnP"
        );
        assert!(
            opts.trackers.is_empty(),
            "high_privacy 不得注入全局 tracker"
        );
    }

    #[test]
    fn test_high_privacy_skips_socks_https_tracker_injection() {
        let mut config = MagnetConfig::default();
        config.high_privacy = true;
        config.socks_proxy_url = Some("socks5://127.0.0.1:1080".into());
        config.trackers = vec!["udp://tracker.example:6969/announce".into()];
        let (opts, _effective, _source) = BtSession::build_session_options(&config);
        assert!(opts.disable_dht);
        assert!(
            opts.trackers.is_empty(),
            "high_privacy+SOCKS 不得追加 HTTPS 公共 tracker"
        );
    }

    #[test]
    fn test_socks_filters_udp_trackers_and_appends_https() {
        // SOCKS5 启用(通过显式配置,确定性,不依赖环境变量)
        let mut config = test_config();
        config.socks_proxy_url = Some("socks5://127.0.0.1:1080".into());
        config.disable_dht_when_socks = true;
        config.trackers = vec![
            "udp://tracker.opentrackr.org:1337/announce".into(),
            "https://tracker.example.org:443/announce".into(),
        ];

        let (opts, _effective, _source) = BtSession::build_session_options(&config);

        // SOCKS5 代理 URL 注入
        assert_eq!(
            opts.socks_proxy_url.as_deref(),
            Some("socks5://127.0.0.1:1080"),
            "socks_proxy_url 应注入到 opts"
        );
        // DHT 在 SOCKS5 + disable_dht_when_socks=true 下禁用
        assert!(
            opts.disable_dht,
            "SOCKS5 + disable_dht_when_socks=true 应禁用 DHT"
        );
        // UDP tracker 被过滤
        let has_udp = opts.trackers.iter().any(|u| u.scheme() == "udp");
        assert!(
            !has_udp,
            "SOCKS5 启用时 UDP tracker 应被过滤(不可达),实际仍含 UDP"
        );
        // 原有 HTTPS tracker 保留
        assert!(
            opts.trackers
                .iter()
                .any(|u| u.as_str().contains("tracker.example.org")),
            "原有 HTTPS tracker 应保留"
        );
        // 追加的 HTTPS tracker 存在
        assert!(
            opts.trackers
                .iter()
                .any(|u| u.as_str().contains("tracker.tamersunion.org")),
            "SOCKS5 启用应追加 HTTPS tracker(tamersunion)"
        );
        assert!(
            opts.trackers
                .iter()
                .any(|u| u.as_str().contains("tracker.gbitt.info")),
            "SOCKS5 启用应追加 HTTPS tracker(gbitt)"
        );
    }

    #[test]
    fn test_socks_keeps_dht_when_disable_dht_when_socks_false() {
        let mut config = test_config();
        config.enable_dht = true;
        config.socks_proxy_url = Some("socks5://127.0.0.1:1080".into());
        config.disable_dht_when_socks = false;

        let (opts, _effective, _source) = BtSession::build_session_options(&config);

        assert!(
            !opts.disable_dht,
            "SOCKS5 启用但 disable_dht_when_socks=false 且 enable_dht=true 时 DHT 不应禁用"
        );
    }

    #[test]
    fn test_default_config_has_peer_opts_and_defer_writes() {
        // 默认配置应产出非空 peer_opts 与非 None defer_writes
        let config = MagnetConfig::default();
        let (opts, _effective, _source) = BtSession::build_session_options(&config);
        let peer_opts = opts.peer_opts.expect("默认配置 peer_opts 应为 Some");
        assert_eq!(
            peer_opts.connect_timeout,
            Some(Duration::from_secs(8)),
            "默认 peer_connect_timeout_secs=8"
        );
        assert_eq!(
            peer_opts.read_write_timeout,
            Some(Duration::from_secs(10)),
            "默认 peer_read_write_timeout_secs=10"
        );
        assert_eq!(
            opts.defer_writes_up_to,
            Some(16),
            "默认 defer_writes_up_to_mb=16"
        );
    }

    #[tokio::test]
    async fn test_bt_session_new_constructs_without_panic() {
        // 端到端:build_session_options 产出的 opts 能被 Session 接受
        let dir = tempfile::TempDir::new().unwrap();
        let config = test_config();
        let session = BtSession::new(dir.path().to_path_buf(), config).await;
        assert!(session.is_ok(), "BtSession 应创建成功: {:?}", session.err());
        // 覆盖 getter:session / config / download_dir / handle_cache / ops_gate /
        // proxy_coverage_status / socks_source / effective_socks_redacted
        let session = session.unwrap();
        let _session_arc = session.session();
        let _config_ref = session.config();
        let _download_dir = session.download_dir();
        let _handle_cache = session.handle_cache();
        let _ops_gate = session.ops_gate();
        let _proxy_report = session.proxy_coverage_status();
        let _socks_source = session.socks_source();
        let _effective_redacted = session.effective_socks_redacted();
    }

    #[test]
    fn test_redact_socks_proxy_strips_credentials_keeps_host_port() {
        // 含凭据的 SOCKS5 URL:脱敏后不应含凭据,但保留 host:port
        let proxy = "socks5://user:pass@127.0.0.1:1080";
        let redacted = redact_socks_proxy_for_log(proxy);
        assert!(
            !redacted.contains("user"),
            "脱敏后不应含 username,实际: {redacted}"
        );
        assert!(
            !redacted.contains("pass"),
            "脱敏后不应含 password,实际: {redacted}"
        );
        assert!(
            redacted.contains("127.0.0.1:1080"),
            "脱敏后应保留 host:port,实际: {redacted}"
        );
        assert!(
            redacted.starts_with("socks5://"),
            "脱敏后应保留 scheme,实际: {redacted}"
        );
        // 精确断言整体形态
        assert_eq!(redacted, "socks5://127.0.0.1:1080");
    }

    #[test]
    fn test_redact_socks_proxy_without_credentials_unchanged() {
        // 无凭据的 SOCKS5 URL:脱敏后形态不变(幂等)
        let proxy = "socks5://127.0.0.1:1080";
        let redacted = redact_socks_proxy_for_log(proxy);
        assert_eq!(redacted, "socks5://127.0.0.1:1080");
    }

    #[test]
    fn test_redact_socks_proxy_invalid_url_returns_placeholder() {
        // 非法 URL:返回固定占位符,绝不泄漏原始输入
        let invalid = "not a url at all :::";
        let redacted = redact_socks_proxy_for_log(invalid);
        assert_eq!(redacted, "<invalid proxy url>");
        assert!(
            !redacted.contains(invalid),
            "非法 URL 时不应回显原始输入(可能含凭据)"
        );
    }

    // ── FIX-16: BT 代理流量覆盖状态(隐私可见性) ──────────────

    /// FIX-16:审计指出 BT SOCKS 代理的「全流量覆盖」缺乏证据(应用侧措施已存在,
    /// 但 librqbit 内部对 peer TCP / HTTP(S) tracker / UDP tracker / DHT / uTP / UPnP 各路径
    /// 是否走 SOCKS 不可从应用代码证明)。本函数在应用层对已配置状态做可见性汇总,
    /// 供前端展示隐私边界,让用户知晓哪些流量经代理、哪些可能绕过。
    #[test]
    fn test_bt_proxy_coverage_no_socks_all_direct() {
        let mut config = test_config();
        config.socks_proxy_url = None;
        config.enable_dht = true;
        config.enable_upnp = false;
        let report = bt_proxy_coverage_status(&config);
        assert_eq!(report.peer_tcp, ProxyCoverage::Direct);
        assert_eq!(report.http_tracker, ProxyCoverage::Direct);
        assert_eq!(report.udp_tracker_dht, ProxyCoverage::Direct);
        assert_eq!(report.utp, ProxyCoverage::Direct);
        assert!(!report.socks_enabled);
    }

    #[test]
    fn test_bt_proxy_coverage_socks_proxies_core_and_blocks_udp_dht() {
        let mut config = test_config();
        config.socks_proxy_url = Some("socks5://127.0.0.1:1080".into());
        config.disable_dht_when_socks = true;
        config.enable_upnp = true;
        let report = bt_proxy_coverage_status(&config);
        assert!(report.socks_enabled);
        assert_eq!(report.peer_tcp, ProxyCoverage::ViaProxy);
        assert_eq!(report.http_tracker, ProxyCoverage::ViaProxy);
        assert_eq!(report.udp_tracker_dht, ProxyCoverage::Blocked);
        assert_eq!(report.utp, ProxyCoverage::MayBypass);
        assert_eq!(report.upnp, ProxyCoverage::MayBypass);
    }

    #[test]
    fn test_bt_proxy_coverage_socks_keeps_dht_when_not_disabled() {
        let mut config = test_config();
        config.socks_proxy_url = Some("socks5://127.0.0.1:1080".into());
        config.disable_dht_when_socks = false;
        config.enable_dht = true;
        let report = bt_proxy_coverage_status(&config);
        assert_eq!(report.udp_tracker_dht, ProxyCoverage::MayBypass);
    }

    #[test]
    fn test_bt_proxy_coverage_upnp_off_when_disabled() {
        let mut config = test_config();
        config.socks_proxy_url = Some("socks5://127.0.0.1:1080".into());
        config.enable_upnp = false;
        let report = bt_proxy_coverage_status(&config);
        assert_eq!(report.upnp, ProxyCoverage::Disabled);
    }

    /// 审计 A-09:effective SOCKS(环境检测)在 config 显式 None 时仍标记 ViaProxy
    #[test]
    fn test_a09_effective_env_socks_enables_coverage() {
        let mut config = test_config();
        config.socks_proxy_url = None;
        config.disable_dht_when_socks = true;
        config.enable_upnp = false;
        let report = bt_proxy_coverage_status_effective(
            &config,
            true,
            SocksProxySource::Environment,
            Some("socks5://127.0.0.1:7890".into()),
        );
        assert!(report.socks_enabled);
        assert_eq!(report.socks_source, SocksProxySource::Environment);
        let ep = report.socks_endpoint_redacted.expect("endpoint");
        assert!(ep.contains("127.0.0.1:7890"), "ep={ep}");
        assert_eq!(report.peer_tcp, ProxyCoverage::ViaProxy);
        assert_eq!(report.http_tracker, ProxyCoverage::ViaProxy);
        assert_eq!(report.udp_tracker_dht, ProxyCoverage::Blocked);
    }

    /// 审计 A-09:build_session_options 返回 explicit 来源
    #[test]
    fn test_a09_build_session_options_explicit_source() {
        let mut config = test_config();
        config.socks_proxy_url = Some("socks5://user:pass@127.0.0.1:1080".into());
        let (opts, effective, source) = BtSession::build_session_options(&config);
        assert_eq!(source, SocksProxySource::Explicit);
        assert_eq!(
            effective.as_deref(),
            Some("socks5://user:pass@127.0.0.1:1080")
        );
        assert_eq!(
            opts.socks_proxy_url.as_deref(),
            Some("socks5://user:pass@127.0.0.1:1080")
        );
        // 报告脱敏
        let report = bt_proxy_coverage_status_effective(
            &config,
            true,
            source,
            effective.as_deref().map(redact_socks_proxy_for_log),
        );
        let ep = report.socks_endpoint_redacted.expect("redacted");
        assert!(ep.contains("127.0.0.1:1080"), "ep={ep}");
        assert!(!ep.contains("user"), "脱敏后不得含用户名: {ep}");
        assert!(!ep.contains("pass"), "脱敏后不得含密码: {ep}");
    }

    /// BT-15:enable_upnp=true 时 MUST 自动设 listen_port_range(默认 6881..6889),
    /// 否则 librqbit 不创建 TCP listener,UPnP 静默不启动,无入站 peer。
    #[test]
    fn test_build_session_options_sets_listen_port_when_upnp_enabled() {
        let mut config = MagnetConfig::default();
        config.enable_upnp = true;
        let (opts, _eff, _src) = BtSession::build_session_options(&config);
        assert!(
            opts.listen_port_range.is_some(),
            "enable_upnp=true 时 MUST 设 listen_port_range"
        );
        let range = opts.listen_port_range.unwrap();
        assert_eq!(range.start, 6881, "默认 BT listen 端口起始 6881");
        assert_eq!(range.end, 6889, "默认 BT listen 端口结束 6889");
    }

    /// BT-15:enable_upnp=false(默认)时 MUST 不设 listen_port_range(不主动开 listener)。
    #[test]
    fn test_build_session_options_no_listen_port_when_upnp_disabled() {
        let config = MagnetConfig::default();
        // 默认 enable_upnp=false
        assert!(!config.enable_upnp);
        let (opts, _eff, _src) = BtSession::build_session_options(&config);
        assert!(
            opts.listen_port_range.is_none(),
            "enable_upnp=false 时 MUST 不设 listen_port_range"
        );
    }

    /// BT-15:high_privacy=true 优先,即使 enable_upnp=true 也不设 listen_port_range。
    #[test]
    fn test_build_session_options_high_privacy_overrides_upnp() {
        let mut config = MagnetConfig::default();
        config.high_privacy = true;
        config.enable_upnp = true;
        let (opts, _eff, _src) = BtSession::build_session_options(&config);
        assert!(
            opts.listen_port_range.is_none(),
            "high_privacy=true 优先,不设 listen_port_range"
        );
        assert!(
            !opts.enable_upnp_port_forwarding,
            "high_privacy=true 同时禁用 UPnP forwarding"
        );
    }
}
