//! 连接池管理
//!
//! 每个主机维护独立连接池,支持连接复用和并发控制。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use tokio::sync::Semaphore;

use amd_core::AmdError;
use amd_core::config::ConnectionConfig;

/// 连接池配置
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// 单主机最大连接数
    pub max_per_host: u32,
    /// 全局最大连接数
    pub max_global: u32,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_per_host: 16,
            max_global: 256,
        }
    }
}

/// 从 amd-core 的 ConnectionConfig 转换为连接池配置
impl From<ConnectionConfig> for PoolConfig {
    fn from(config: ConnectionConfig) -> Self {
        Self {
            max_per_host: config.max_connections_per_host,
            max_global: config.max_global_connections,
        }
    }
}

/// 从连接池配置转换为 amd-core 的 ConnectionConfig
impl From<PoolConfig> for ConnectionConfig {
    fn from(config: PoolConfig) -> Self {
        Self {
            max_connections_per_host: config.max_per_host,
            max_global_connections: config.max_global,
            ..ConnectionConfig::default()
        }
    }
}

/// 全局连接池管理器
pub struct ConnectionPool {
    config: PoolConfig,
    pub(crate) global_semaphore: Arc<Semaphore>,
    active_count: Arc<AtomicU32>,
    host_semaphores: tokio::sync::Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl ConnectionPool {
    /// 创建新的连接池
    pub fn new(config: PoolConfig) -> Self {
        Self {
            global_semaphore: Arc::new(Semaphore::new(config.max_global as usize)),
            config,
            active_count: Arc::new(AtomicU32::new(0)),
            host_semaphores: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// 获取主机级别的信号量
    async fn host_semaphore(&self, host: &str) -> Arc<Semaphore> {
        let mut map = self.host_semaphores.lock().await;
        map.entry(host.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(self.config.max_per_host as usize)))
            .clone()
    }

    /// 获取连接许可(全局 + 主机级别双重限制)
    pub async fn acquire(&self, host: &str) -> Result<ConnectionPermit, AmdError> {
        let global_permit = self
            .global_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AmdError::Network("全局连接信号量已关闭".into()))?;
        let host_sem = self.host_semaphore(host).await;
        let host_permit = host_sem
            .acquire_owned()
            .await
            .map_err(|_| AmdError::Network("主机连接信号量已关闭".into()))?;
        self.active_count.fetch_add(1, Ordering::Relaxed);
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
    pub async fn cleanup_idle_hosts(&self) {
        let mut map = self.host_semaphores.lock().await;
        map.retain(|_, sem| sem.available_permits() < self.config.max_per_host as usize);
    }

    /// 当前跟踪的主机数量
    pub async fn host_count(&self) -> usize {
        let map = self.host_semaphores.lock().await;
        map.len()
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
        });
        // 触发主机条目创建
        {
            let _p1 = pool.acquire("example.com").await.unwrap();
            let _p2 = pool.acquire("other.com").await.unwrap();
        }
        // 所有连接已释放,主机应为空闲
        assert_eq!(pool.host_count().await, 2);
        pool.cleanup_idle_hosts().await;
        assert_eq!(pool.host_count().await, 0);
    }

    #[tokio::test]
    async fn test_cleanup_idle_hosts_keeps_active() {
        let pool = ConnectionPool::new(PoolConfig {
            max_per_host: 2,
            max_global: 10,
        });
        let _active = pool.acquire("busy.com").await.unwrap();
        // 空闲主机
        {
            let _p = pool.acquire("idle.com").await.unwrap();
        }
        pool.cleanup_idle_hosts().await;
        // busy.com 仍有活跃连接,应保留;idle.com 应被清理
        assert_eq!(pool.host_count().await, 1);
    }

    #[tokio::test]
    async fn test_cleanup_idle_hosts_empty_pool() {
        let pool = ConnectionPool::new(PoolConfig::default());
        pool.cleanup_idle_hosts().await;
        assert_eq!(pool.host_count().await, 0);
    }

    #[tokio::test]
    async fn test_host_count() {
        let pool = ConnectionPool::new(PoolConfig::default());
        assert_eq!(pool.host_count().await, 0);
        let _p1 = pool.acquire("a.com").await.unwrap();
        let _p2 = pool.acquire("b.com").await.unwrap();
        let _p3 = pool.acquire("c.com").await.unwrap();
        assert_eq!(pool.host_count().await, 3);
    }

    /// 验证信号量关闭时返回错误而非 panic
    #[tokio::test]
    async fn test_semaphore() {
        let pool = ConnectionPool::new(PoolConfig {
            max_per_host: 1,
            max_global: 1,
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
        });
        // 关闭全局信号量
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
        let conn_cfg = amd_core::config::ConnectionConfig {
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
        };
        let conn_cfg: amd_core::config::ConnectionConfig = pool_cfg.into();
        assert_eq!(conn_cfg.max_connections_per_host, 4);
        assert_eq!(conn_cfg.max_global_connections, 64);
        // 未指定的字段应使用默认值
        assert_eq!(conn_cfg.keep_alive_timeout_secs, 30);
        assert_eq!(conn_cfg.connect_timeout_secs, 10);
    }
}
