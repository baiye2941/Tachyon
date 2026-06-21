//! BitTorrent Session 单例管理
//!
//! 类似 ConnectionPool 的全局单例模式，
//! 在 Tauri setup 钩子中创建，随应用生命周期存在。

use std::path::PathBuf;
use std::sync::Arc;

use librqbit::Session;
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
    /// Session 内部管理 DHT、Peer 连接、Piece 下载等生命周期。
    pub async fn new(
        download_dir: PathBuf,
        config: MagnetConfig,
    ) -> tachyon_core::DownloadResult<Self> {
        let session = Session::new(download_dir.clone()).await.map_err(|e| {
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
