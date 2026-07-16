//! 并发限制器(命名沿用 ConnectionPool,实为 per-host + global 信号量许可管理)
//!
//! 本类型不持有/复用 TCP 连接 —— 真正的连接池由底层 reqwest 客户端
//! (`HttpClient` / `HttpClientRegistry`)管理。
//! 其职责是按主机 + 全局两级的并发许可限制:每个主机维护独立信号量,
//! 配合全局信号量控制总并发,避免单主机打满或全局过载。
//! 使用 DashMap 实现无锁主机信号量索引,避免高并发下的锁竞争。
//!
//! 审计 A-02:优先使用类型别名 `ConcurrencyLimiter` 与 `active_requests()`;
//! 历史名 `ConnectionPool` / `active_connections` 保留兼容。

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
    ///
    /// 审计 HTTP-17:**不**驱动 `HttpClient` 的 connect timeout。
    /// HTTP 连接超时唯一 owner 是 `DownloadConfig.connect_timeout_secs`,
    /// 由 engine 构造 HttpClient 时显式传入。本字段仅作 PoolConfig/ConnectionConfig
    /// 互转的配置镜像,避免误以为改 PoolConfig 会改变 HTTP 握手超时。
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

    /// 获取主机级别的信号量
    ///
    /// 审计 H-06:统一走 entry API,避免 get-miss 与 insert 之间被 cleanup 插入另一实例。
    fn host_semaphore(&self, host: &str) -> Arc<Semaphore> {
        self.host_semaphores
            .entry(host.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(self.config.max_per_host as usize)))
            .clone()
    }

    /// 获取连接许可(主机 + 全局双重限制)
    ///
    /// 审计 HTTP-16:必须先 await host 许可,再 await global。
    /// 旧实现先占 global 再等 host,导致 host 饱和的 waiter 占用 global 名额,
    /// 饿死其他空闲 host 的请求(跨 host head-of-line blocking)。
    #[tracing::instrument(skip(self), fields(host = %host))]
    pub async fn acquire(&self, host: &str) -> Result<ConnectionPermit, DownloadError> {
        let host_sem = self.host_semaphore(host);
        let host_permit = host_sem
            .acquire_owned()
            .await
            .map_err(|_| DownloadError::Network("主机连接信号量已关闭".into()))?;
        let global_permit = self
            .global_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| DownloadError::Network("全局连接信号量已关闭".into()))?;
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

    /// 当前活跃请求数(持有许可的请求,非 TCP socket 数)
    ///
    /// 审计 A-02:优先使用本命名;历史 `active_connections` 为兼容别名。
    pub fn active_requests(&self) -> u32 {
        self.active_count.load(Ordering::Relaxed)
    }

    /// 当前活跃连接数(历史名;语义同 `active_requests`)
    pub fn active_connections(&self) -> u32 {
        self.active_requests()
    }

    /// 获取配置
    pub fn config(&self) -> &PoolConfig {
        &self.config
    }

    /// 清理没有活跃请求的主机信号量
    ///
    /// 审计 H-06:仅当 map 是该 `Arc<Semaphore>` 的**唯一**持有者且全部许可可用时才删除。
    /// 若外部仍持有 clone(含:已 acquire 的 permit 内嵌 Arc、await 中的 waiter、或
    /// 刚从 `host_semaphore` clone 尚未 acquire 的引用),删除后同 host 再 insert
    /// 会新建第二个信号量,使 per-host 上限被绕过。
    pub fn cleanup_idle_hosts(&self) {
        let max = self.config.max_per_host as usize;
        self.host_semaphores.retain(|_, sem| {
            // strong_count > 1: 有 permit/waiter/临时 clone,必须保留
            if Arc::strong_count(sem) > 1 {
                return true;
            }
            // 仅 map 持有:仍有未归还许可则保留;全部可用才清理
            sem.available_permits() < max
        });
    }

    /// 当前跟踪的主机数量
    pub fn host_count(&self) -> usize {
        self.host_semaphores.len()
    }

    /// 指定主机活跃请求数(已消耗的许可数)
    ///
    /// 审计 A-02:优先 `host_active_requests`;`host_active_connections` 为兼容别名。
    pub fn host_active_requests(&self, host: &str) -> u32 {
        self.host_semaphores
            .get(host)
            .map(|sem| {
                let used =
                    (self.config.max_per_host as usize).saturating_sub(sem.available_permits());
                used as u32
            })
            .unwrap_or(0)
    }

    /// 指定主机活跃连接数(历史名;语义同 `host_active_requests`)
    pub fn host_active_connections(&self, host: &str) -> u32 {
        self.host_active_requests(host)
    }
}

/// 审计 A-02:诚实类型名 — 并发许可器(非 TCP 连接池)
pub type ConcurrencyLimiter = ConnectionPool;

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

    /// 审计 A-02:ConcurrencyLimiter 别名与 active_requests 一致
    #[tokio::test]
    async fn test_a02_concurrency_limiter_alias_and_active_requests() {
        let limiter: ConcurrencyLimiter = ConnectionPool::new(PoolConfig {
            max_per_host: 2,
            max_global: 4,
            ..Default::default()
        });
        assert_eq!(limiter.active_requests(), 0);
        let _p = limiter.acquire("a.example").await.unwrap();
        assert_eq!(limiter.active_requests(), 1);
        assert_eq!(limiter.active_connections(), 1);
        assert_eq!(limiter.host_active_requests("a.example"), 1);
        assert_eq!(limiter.host_active_connections("a.example"), 1);
    }

    #[tokio::test]
    async fn test_active_connections_with_permits() {
        let pool = ConnectionPool::new(PoolConfig::default());
        let _permit = pool.acquire("example.com").await.unwrap();
        assert_eq!(pool.active_connections(), 1);
    }

    /// 审计 HTTP-16:host 饱和 waiter 不得占住 global 阻塞其他 host。
    /// max_global=2, max_per_host=1:
    /// - A 持有 1 许可
    /// - 第二个 A 等待 host
    /// - B 必须仍能 acquire(不应被 A 的第二个 waiter 占 global 饿死)
    #[tokio::test]
    async fn test_acquire_host_first_avoids_cross_host_hol() {
        use std::sync::Arc;
        use std::time::Duration;
        let pool = Arc::new(ConnectionPool::new(PoolConfig {
            max_per_host: 1,
            max_global: 2,
            ..Default::default()
        }));
        let _a_hold = pool.acquire("host-a").await.expect("A first");
        let pool_a = Arc::clone(&pool);
        let a_waiter = tokio::spawn(async move {
            // 第二个 A:会等 host-a 许可
            pool_a.acquire("host-a").await
        });
        // 让 A waiter 进入等待
        tokio::time::sleep(Duration::from_millis(50)).await;
        // B 必须在超时内拿到许可(若 global-first,A waiter 已占 global,B 可能被饿死当 global=2 且还有别的)
        // 用 max_global=2: A_hold 占 1, 错误实现下 A_waiter 占 global 第 2,B 无法获得。
        let b = tokio::time::timeout(Duration::from_millis(200), pool.acquire("host-b")).await;
        assert!(b.is_ok(), "B 不应被 A 的 host 等待饿死");
        assert!(b.unwrap().is_ok(), "B acquire 应成功");
        // 释放 A 让 waiter 完成,避免 drop 时挂起
        drop(_a_hold);
        let _ = tokio::time::timeout(Duration::from_millis(200), a_waiter).await;
    }

    /// 审计 H-06:持有 host 信号量 Arc 时 cleanup 不得删除该 host
    #[tokio::test]
    async fn test_cleanup_does_not_remove_host_while_arc_held() {
        let pool = ConnectionPool::new(PoolConfig {
            max_per_host: 1,
            max_global: 10,
            enable_http2: false, // 避免默认 16→100 提升干扰
            ..Default::default()
        });
        // 先创建 host 条目
        {
            let _p = pool.acquire("held.com").await.unwrap();
        }
        assert_eq!(pool.host_count(), 1);
        // 外部再 clone 一次(模拟 acquire 前持有/竞态窗口)
        let held = pool.host_semaphore("held.com");
        assert!(Arc::strong_count(&held) >= 2);
        pool.cleanup_idle_hosts();
        assert_eq!(
            pool.host_count(),
            1,
            "外部仍持有 Arc 时 cleanup 不得删除 host 信号量"
        );
        // 与 map 内是同一实例
        let again = pool.host_semaphore("held.com");
        assert!(
            Arc::ptr_eq(&held, &again),
            "cleanup 后同 host 必须仍是同一 Semaphore 实例"
        );
        drop(held);
        drop(again);
        pool.cleanup_idle_hosts();
        assert_eq!(pool.host_count(), 0, "Arc 全释放后 idle host 应可清理");
    }

    /// 审计 H-06:若错误地在 Arc 仍存活时删条目再 insert,会形成双信号量。
    /// 回归:cleanup 后并发 acquire 仍受 max_per_host 约束(通过 host_active 观测)。
    #[tokio::test]
    async fn test_host_semaphore_identity_stable_across_cleanup_race() {
        let pool = Arc::new(ConnectionPool::new(PoolConfig {
            max_per_host: 1,
            max_global: 10,
            enable_http2: false,
            ..Default::default()
        }));
        let sem_before = pool.host_semaphore("race.com");
        // 并发:一端持有 clone 并 cleanup,一端反复 host_semaphore
        let pool2 = Arc::clone(&pool);
        let join = tokio::spawn(async move {
            for _ in 0..50 {
                pool2.cleanup_idle_hosts();
                let _ = pool2.host_semaphore("race.com");
                tokio::task::yield_now().await;
            }
        });
        for _ in 0..50 {
            let s = pool.host_semaphore("race.com");
            assert!(
                Arc::ptr_eq(&sem_before, &s),
                "持有外部 clone 期间同 host 不得换新 Semaphore"
            );
            pool.cleanup_idle_hosts();
            tokio::task::yield_now().await;
        }
        join.await.unwrap();
        drop(sem_before);
        // 全部释放后可清理
        pool.cleanup_idle_hosts();
        assert_eq!(pool.host_count(), 0);
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

    #[tokio::test]
    async fn test_host_active_connections_tracks_permits() {
        // 回归测试 F-08:host_active_connections 必须返回有界、正确的值,
        // 不得因 available_permits 与 max_per_host 失配而下溢 panic 或 wrap。
        let pool = ConnectionPool::new(PoolConfig {
            max_per_host: 3,
            max_global: 10,
            ..Default::default()
        });

        // 未知主机返回 0
        assert_eq!(pool.host_active_connections("unknown.com"), 0);

        // 持有两个 permit 时,活跃数 = max_per_host - available_permits = 3 - 1 = 2
        let _p1 = pool.acquire("example.com").await.unwrap();
        let _p2 = pool.acquire("example.com").await.unwrap();
        assert_eq!(pool.host_active_connections("example.com"), 2);

        // 释放一个后回到 1
        drop(_p2);
        assert_eq!(pool.host_active_connections("example.com"), 1);

        // 全部释放后回到 0,且不超过 max_per_host
        drop(_p1);
        let active = pool.host_active_connections("example.com");
        assert!(
            active <= 3,
            "活跃连接数不应超过 max_per_host,实际: {active}"
        );
    }
}
