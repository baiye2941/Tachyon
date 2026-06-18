//! 连接池管理
//!
//! 每个主机维护独立连接池,支持连接复用和并发控制。
//! 使用 DashMap 实现无锁主机信号量索引,避免高并发下的锁竞争。

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use dashmap::DashMap;
use tokio::sync::Semaphore;

use tachyon_core::DownloadError;
use tachyon_core::config::ConnectionConfig;

/// 连接池配置
///
/// 与 `tachyon_core::config::ConnectionConfig` 字段对齐,
/// 连接池据此控制并发、Keep-Alive 和协议启用策略。
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// 单主机最大连接数
    pub max_per_host: u32,
    /// 全局最大连接数
    pub max_global: u32,
    /// Keep-Alive 超时(秒)
    pub keep_alive_timeout_secs: u64,
    /// 连接建立超时(秒)
    pub connect_timeout_secs: u64,
    /// 是否启用 HTTP/2
    pub enable_http2: bool,
    /// 是否启用 QUIC
    pub enable_quic: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_per_host: 16,
            max_global: 256,
            keep_alive_timeout_secs: 30,
            connect_timeout_secs: 10,
            enable_http2: true,
            enable_quic: false,
        }
    }
}

/// 从 tachyon-core 的 ConnectionConfig 转换为连接池配置
impl From<ConnectionConfig> for PoolConfig {
    fn from(config: ConnectionConfig) -> Self {
        Self {
            max_per_host: config.max_connections_per_host,
            max_global: config.max_global_connections,
            keep_alive_timeout_secs: config.keep_alive_timeout_secs,
            connect_timeout_secs: config.connect_timeout_secs,
            enable_http2: config.enable_http2,
            enable_quic: config.enable_quic,
        }
    }
}

/// 从连接池配置转换为 tachyon-core 的 ConnectionConfig
impl From<PoolConfig> for ConnectionConfig {
    fn from(config: PoolConfig) -> Self {
        Self {
            max_connections_per_host: config.max_per_host,
            max_global_connections: config.max_global,
            keep_alive_timeout_secs: config.keep_alive_timeout_secs,
            connect_timeout_secs: config.connect_timeout_secs,
            enable_http2: config.enable_http2,
            enable_quic: config.enable_quic,
        }
    }
}

/// 全局连接池管理器
pub struct ConnectionPool {
    config: PoolConfig,
    pub(crate) global_semaphore: Arc<Semaphore>,
    active_count: Arc<AtomicU32>,
    host_semaphores: DashMap<String, Arc<Semaphore>>,
    /// host_semaphores 自动清理阈值:超过此数量时在 acquire 中触发清理
    cleanup_threshold: usize,
}

impl ConnectionPool {
    /// 创建新的连接池
    ///
    /// HTTP/2 模式下,若 `max_per_host` 保持默认值(16),自动提升到 100:
    /// 单个 TCP 连接可多路复用 100+ 流,默认 16 的信号量限制会人为制造瓶颈。
    /// 用户显式设置的值始终被尊重。
    pub fn new(mut config: PoolConfig) -> Self {
        const DEFAULT_MAX_PER_HOST: u32 = 16;
        const HTTP2_RECOMMENDED_PER_HOST: u32 = 100;
        // 仅当用户未显式修改 max_per_host 时自动提升
        if config.enable_http2 && config.max_per_host == DEFAULT_MAX_PER_HOST {
            config.max_per_host = HTTP2_RECOMMENDED_PER_HOST;
        }
        // 清理阈值 = max_global * 2,避免 host_semaphores 无限增长
        let cleanup_threshold = (config.max_global as usize).saturating_mul(2).max(64);
        Self {
            global_semaphore: Arc::new(Semaphore::new(config.max_global as usize)),
            config,
            active_count: Arc::new(AtomicU32::new(0)),
            host_semaphores: DashMap::new(),
            cleanup_threshold,
        }
    }

    /// 获取主机级别的信号量(无锁读取)
    fn host_semaphore(&self, host: &str) -> Arc<Semaphore> {
        if let Some(sem) = self.host_semaphores.get(host) {
            return sem.clone();
        }
        self.host_semaphores
            .entry(host.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(self.config.max_per_host as usize)))
            .clone()
    }

    /// 获取连接许可(全局 + 主机级别双重限制)
    #[tracing::instrument(skip(self), fields(host = %host))]
    pub async fn acquire(&self, host: &str) -> Result<ConnectionPermit, DownloadError> {
        let global_permit = self
            .global_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| DownloadError::Network("全局连接信号量已关闭".into()))?;
        let host_sem = self.host_semaphore(host);
        let host_permit = host_sem
            .acquire_owned()
            .await
            .map_err(|_| DownloadError::Network("主机连接信号量已关闭".into()))?;
        self.active_count.fetch_add(1, Ordering::Relaxed);

        // W-09: 当主机信号量条目超过阈值时自动清理空闲条目,防止内存泄漏
        if self.host_semaphores.len() > self.cleanup_threshold {
            self.cleanup_idle_hosts();
        }

        Ok(ConnectionPermit {
            _global_permit: global_permit,
            _host_permit: host_permit,
            active_count: Arc::clone(&self.active_count),
        })
    }

    /// 当前活跃连接数
    pub fn active_connections(&self) -> u32 {
        self.active_count.load(Ordering::Relaxed)
    }

    /// 获取配置
    pub fn config(&self) -> &PoolConfig {
        &self.config
    }

    /// 清理没有活跃连接的主机信号量
    ///
    /// 遍历所有主机信号量,移除那些所有许可都可用(即无活跃连接)的条目。
    /// 建议在下载任务完成后定期调用,避免内存泄漏。
    pub fn cleanup_idle_hosts(&self) {
        self.host_semaphores
            .retain(|_, sem| sem.available_permits() < self.config.max_per_host as usize);
    }

    /// 当前跟踪的主机数量
    pub fn host_count(&self) -> usize {
        self.host_semaphores.len()
    }

    /// A-05: 获取指定主机的活跃连接数(已消耗的许可数)
    ///
    /// 返回 `max_per_host - available_permits`,即当前正在使用的连接数。
    /// 用于监控和诊断连接池状态。
    pub fn host_active_connections(&self, host: &str) -> u32 {
        self.host_semaphores
            .get(host)
            .map(|sem| {
                let used = self.config.max_per_host as usize - sem.available_permits();
                used as u32
            })
            .unwrap_or(0)
    }
}

/// 连接许可,Drop 时自动归还连接
pub struct ConnectionPermit {
    _global_permit: tokio::sync::OwnedSemaphorePermit,
    _host_permit: tokio::sync::OwnedSemaphorePermit,
    active_count: Arc<AtomicU32>,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.active_count.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn test_pool_creation() {
        let pool = ConnectionPool::new(PoolConfig::default());
        assert_eq!(pool.active_connections(), 0);
    }

    #[tokio::test]
    async fn test_acquire_and_release() {
        let pool = ConnectionPool::new(PoolConfig {
            max_per_host: 2,
            max_global: 10,
            ..Default::default()
        });
        {
            let _permit = pool.acquire("example.com").await.unwrap();
            assert_eq!(pool.active_connections(), 1);
        }
        assert_eq!(pool.active_connections(), 0);
    }

    #[tokio::test]
    async fn test_host_limit() {
        let pool = Arc::new(ConnectionPool::new(PoolConfig {
            max_per_host: 2,
            max_global: 10,
            ..Default::default()
        }));
        let _p1 = pool.acquire("example.com").await.unwrap();
        let _p2 = pool.acquire("example.com").await.unwrap();
        assert_eq!(pool.active_connections(), 2);
    }

    #[tokio::test]
    async fn test_different_hosts_independent() {
        let pool = ConnectionPool::new(PoolConfig {
            max_per_host: 1,
            max_global: 10,
            ..Default::default()
        });
        let _p1 = pool.acquire("host1.com").await.unwrap();
        let _p2 = pool.acquire("host2.com").await.unwrap();
        assert_eq!(pool.active_connections(), 2);
    }

    #[test]
    fn test_default_config() {
        let config = PoolConfig::default();
        assert_eq!(config.max_per_host, 16);
        assert_eq!(config.max_global, 256);
    }

    #[tokio::test]
    async fn test_cleanup_idle_hosts_removes_inactive() {
        let pool = ConnectionPool::new(PoolConfig {
            max_per_host: 2,
            max_global: 10,
            ..Default::default()
        });
        {
            let _p1 = pool.acquire("example.com").await.unwrap();
            let _p2 = pool.acquire("other.com").await.unwrap();
        }
        assert_eq!(pool.host_count(), 2);
        pool.cleanup_idle_hosts();
        assert_eq!(pool.host_count(), 0);
    }

    #[tokio::test]
    async fn test_cleanup_idle_hosts_keeps_active() {
        let pool = ConnectionPool::new(PoolConfig {
            max_per_host: 2,
            max_global: 10,
            ..Default::default()
        });
        let _active = pool.acquire("busy.com").await.unwrap();
        {
            let _p = pool.acquire("idle.com").await.unwrap();
        }
        pool.cleanup_idle_hosts();
        assert_eq!(pool.host_count(), 1);
    }

    #[tokio::test]
    async fn test_cleanup_idle_hosts_empty_pool() {
        let pool = ConnectionPool::new(PoolConfig::default());
        pool.cleanup_idle_hosts();
        assert_eq!(pool.host_count(), 0);
    }

    #[tokio::test]
    async fn test_host_count() {
        let pool = ConnectionPool::new(PoolConfig::default());
        assert_eq!(pool.host_count(), 0);
        let _p1 = pool.acquire("a.com").await.unwrap();
        let _p2 = pool.acquire("b.com").await.unwrap();
        let _p3 = pool.acquire("c.com").await.unwrap();
        assert_eq!(pool.host_count(), 3);
    }

    #[tokio::test]
    async fn test_semaphore() {
        let pool = ConnectionPool::new(PoolConfig {
            max_per_host: 1,
            max_global: 1,
            ..Default::default()
        });
        pool.global_semaphore.close();
        let result = pool.acquire("test.com").await;
        assert!(result.is_err(), "关闭的信号量应返回错误而非 panic");
        let err = match result {
            Ok(_) => panic!("期望错误"),
            Err(e) => e,
        };
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("信号量") || err_msg.contains("semaphore"),
            "错误信息应包含信号量描述: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_semaphore_closed_returns_error() {
        let pool = ConnectionPool::new(PoolConfig {
            max_per_host: 1,
            max_global: 1,
            ..Default::default()
        });
        pool.global_semaphore.close();
        let result = pool.acquire("test.com").await;
        assert!(result.is_err(), "关闭的信号量应返回错误而非 panic");
        let err = match result {
            Ok(_) => panic!("期望错误"),
            Err(e) => e,
        };
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("信号量") || err_msg.contains("semaphore"),
            "错误信息应包含信号量相关描述,实际: {err_msg}"
        );
    }

    #[test]
    fn test_pool_config_from_connection_config() {
        let conn_cfg = tachyon_core::config::ConnectionConfig {
            max_connections_per_host: 8,
            max_global_connections: 128,
            keep_alive_timeout_secs: 60,
            connect_timeout_secs: 5,
            enable_http2: true,
            enable_quic: true,
        };
        let pool_cfg: PoolConfig = conn_cfg.into();
        assert_eq!(pool_cfg.max_per_host, 8);
        assert_eq!(pool_cfg.max_global, 128);
    }

    #[test]
    fn test_connection_config_from_pool_config() {
        let pool_cfg = PoolConfig {
            max_per_host: 4,
            max_global: 64,
            ..Default::default()
        };
        let conn_cfg: tachyon_core::config::ConnectionConfig = pool_cfg.into();
        assert_eq!(conn_cfg.max_connections_per_host, 4);
        assert_eq!(conn_cfg.max_global_connections, 64);
        assert_eq!(conn_cfg.keep_alive_timeout_secs, 30);
        assert_eq!(conn_cfg.connect_timeout_secs, 10);
    }

    #[tokio::test]
    async fn test_active_connections_initial() {
        let pool = ConnectionPool::new(PoolConfig::default());
        assert_eq!(pool.active_connections(), 0);
    }

    #[tokio::test]
    async fn test_active_connections_with_permits() {
        let pool = ConnectionPool::new(PoolConfig::default());
        let _permit = pool.acquire("example.com").await.unwrap();
        assert_eq!(pool.active_connections(), 1);
    }

    #[tokio::test]
    async fn test_max_global_blocks_new_acquire() {
        let pool = Arc::new(ConnectionPool::new(PoolConfig {
            max_per_host: 10,
            max_global: 2,
            ..Default::default()
        }));

        let _p1 = pool.acquire("host1.com").await.unwrap();
        let _p2 = pool.acquire("host2.com").await.unwrap();
        assert_eq!(pool.active_connections(), 2);

        let pool2 = Arc::clone(&pool);
        let handle = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_millis(50), pool2.acquire("host3.com")).await
        });

        let result = handle.await.unwrap();
        assert!(
            result.is_err(),
            "全局并发满时新请求应被阻塞,不应在 50ms 内获得许可"
        );
    }

    #[test]
    fn test_http2_auto_increases_default_max_per_host() {
        let pool = ConnectionPool::new(PoolConfig {
            enable_http2: true,
            max_per_host: 16,
            ..Default::default()
        });
        assert_eq!(
            pool.config().max_per_host,
            100,
            "HTTP/2 默认值应自动从 16 提升到 100"
        );
    }

    #[test]
    fn test_http2_keeps_explicit_max_per_host() {
        let pool = ConnectionPool::new(PoolConfig {
            enable_http2: true,
            max_per_host: 32,
            ..Default::default()
        });
        assert_eq!(
            pool.config().max_per_host,
            32,
            "用户显式设置的 max_per_host 应被尊重"
        );
    }

    #[test]
    fn test_http2_disabled_keeps_default_max_per_host() {
        let pool = ConnectionPool::new(PoolConfig {
            enable_http2: false,
            max_per_host: 16,
            ..Default::default()
        });
        assert_eq!(
            pool.config().max_per_host,
            16,
            "未启用 HTTP/2 时不应自动提升"
        );
    }

    #[tokio::test]
    async fn test_host_semaphores_auto_cleanup_at_threshold() {
        let pool = Arc::new(ConnectionPool::new(PoolConfig {
            max_per_host: 1,
            max_global: 2, // cleanup_threshold = max(2*2, 64) = 64
            ..Default::default()
        }));

        // 创建 65 个主机,在第 65 次 acquire 时触发阈值清理
        for i in 0..65 {
            let host = format!("host{i}.com");
            let _permit = pool.acquire(&host).await.unwrap();
        }

        // 第 65 次 acquire 触发清理后,仅保留当前活跃主机,
        // 其余 64 个已释放的空闲主机信号量应被移除
        assert_eq!(pool.host_count(), 1, "超过阈值时应自动清理空闲主机信号量");

        // 继续增加主机使总数再次超过阈值,验证清理持续生效
        for i in 65..129 {
            let host = format!("host{i}.com");
            let _permit = pool.acquire(&host).await.unwrap();
        }
        assert_eq!(pool.host_count(), 1, "再次超过阈值后仍应只保留当前活跃主机");
    }

    #[tokio::test]
    async fn test_host_semaphores_no_cleanup_below_threshold() {
        let pool = Arc::new(ConnectionPool::new(PoolConfig {
            max_per_host: 1,
            max_global: 100, // cleanup_threshold = 200
            ..Default::default()
        }));

        for i in 0..10 {
            let host = format!("host{i}.com");
            let _permit = pool.acquire(&host).await.unwrap();
        }

        assert_eq!(
            pool.host_count(),
            10,
            "低于清理阈值时不应自动清理主机信号量"
        );
    }

    #[test]
    fn test_permit_drop_releases_on_panic() {
        let pool = Arc::new(ConnectionPool::new(PoolConfig {
            max_per_host: 1,
            max_global: 1,
            ..Default::default()
        }));

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe({
            let pool = Arc::clone(&pool);
            move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    let _permit = pool.acquire("test.com").await.unwrap();
                    assert_eq!(pool.active_connections(), 1);
                    panic!("deliberate panic");
                });
            }
        }));

        assert!(result.is_err(), "应有意触发 panic");
        assert_eq!(
            pool.active_connections(),
            0,
            "panic 后 permit 应通过 Drop 释放活跃计数"
        );
    }
}
