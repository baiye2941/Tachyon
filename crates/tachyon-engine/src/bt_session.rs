//! BitTorrent Session 单例管理
//!
//! 类似 ConnectionPool 的全局单例模式，
//! 在 Tauri setup 钩子中创建，随应用生命周期存在。

use std::path::PathBuf;
use std::sync::Arc;

use librqbit::{Session, SessionOptions};
use tachyon_core::config::MagnetConfig;

/// BitTorrent Session 单例
///
/// 封装 librqbit Session，提供全局共享的 BitTorrent 引擎实例。
/// 在 tachyon-app 的 Tauri setup 钩子中创建，通过 Arc 共享注入。
pub struct BtSession {
    inner: Arc<Session>,
    config: MagnetConfig,
    download_dir: PathBuf,
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
        let mut opts = SessionOptions {
            disable_dht: !config.enable_dht,
            enable_upnp_port_forwarding: config.enable_upnp,
            disable_dht_persistence: config.disable_dht_persistence,
            ..Default::default()
        };

        // SOCKS5 代理:优先用户手动配置,None 时自动检测系统代理
        // 让 BT tracker(reqwest)和 peer TCP(StreamConnector)走代理,
        // 国内访问国外 BT 资源必需(UDP tracker/DHT 仍直连,socks5 不代理 UDP)
        let socks_proxy = config.socks_proxy_url.clone().or_else(|| {
            tachyon_core::config::detect_socks_proxy().inspect(|proxy| {
                tracing::info!(proxy = %proxy, "自动检测到系统 SOCKS5 代理(BT tracker+peer 将走代理)");
            })
        });
        if let Some(ref proxy) = socks_proxy {
            opts.socks_proxy_url = Some(proxy.clone());
            tracing::info!(proxy = %proxy, "BT SOCKS5 代理已启用");
        }

        // 全局 tracker: 附加到每个磁力链接的 tracker 列表，
        // 即使磁力链接本身不包含 tracker 也能快速发现 peer。
        for tracker_url in &config.trackers {
            if let Ok(url) = url::Url::parse(tracker_url) {
                opts.trackers.insert(url);
            }
        }

        let session = Session::new_with_opts(download_dir.clone(), opts)
            .await
            .map_err(|e| {
                tachyon_core::DownloadError::Config(format!("创建 BitTorrent Session 失败: {e}"))
            })?;

        Ok(Self {
            inner: session,
            config,
            download_dir,
        })
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
}
