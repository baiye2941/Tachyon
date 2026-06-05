//! Tachyon 核心类型、trait 定义与错误体系
//!
//! 本 crate 定义所有模块共享的公共接口,包括:
//! - 下载任务、分片、协议、存储、校验的 trait 抽象
//! - 统一错误类型
//! - 配置类型
//! - 事件类型

pub mod config;
pub mod error;
pub mod event;
pub mod filename;
pub mod test_harness;
pub mod traits;
pub mod types;
pub mod url_safety;

use std::sync::atomic::{AtomicU64, Ordering};

// 重新导出核心类型
pub use config::AppConfig;
pub use config::SchedulerConfig;
pub use config::USER_AGENT;
pub use error::{DownloadError, DownloadResult};
pub use filename::{
    extract_filename, extract_filename_from_url, parse_content_disposition, sanitize_filename,
    validate_save_path,
};
pub use traits::{ByteStream, Protocol, Storage, Verifier};
pub use types::{
    DownloadState, DownloadStateChange, FileMetadata, FragmentInfo, TaskId, TaskProgress,
};
pub use url_safety::{redact_url_for_log, reject_forbidden_ip, validate_public_http_url};

/// 下载性能指标计数器
///
/// 使用 AtomicU64 实现无锁统计,适用于高并发下载场景。
/// 各字段含义:
/// - `bytes_downloaded`: 累计已下载字节数
/// - `fragments_completed`: 已完成的分片数
/// - `errors`: 错误计数
#[derive(Debug)]
pub struct Metrics {
    /// 累计已下载字节数
    pub bytes_downloaded: AtomicU64,
    /// 已完成的分片数
    pub fragments_completed: AtomicU64,
    /// 错误计数
    pub errors: AtomicU64,
}

impl Metrics {
    /// 创建全零初始化的指标实例
    pub fn new() -> Self {
        Self {
            bytes_downloaded: AtomicU64::new(0),
            fragments_completed: AtomicU64::new(0),
            errors: AtomicU64::new(0),
        }
    }

    /// 原子累加下载字节数
    pub fn add_bytes(&self, n: u64) {
        self.bytes_downloaded.fetch_add(n, Ordering::Relaxed);
    }

    /// 原子递增完成分片数
    pub fn inc_fragment(&self) {
        self.fragments_completed.fetch_add(1, Ordering::Relaxed);
    }

    /// 原子递增错误计数
    pub fn inc_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// 高性能 hex 编码(预分配数组,无逐字节 format! 分配)
///
/// 将字节数组编码为十六进制字符串,使用查表法避免逐字节分配。
/// 性能比 `format!("{:02x}", byte)` 循环快约 5 倍。
pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX_TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut buf = vec![0u8; bytes.len() * 2];
    for (i, &b) in bytes.iter().enumerate() {
        buf[i * 2] = HEX_TABLE[(b >> 4) as usize];
        buf[i * 2 + 1] = HEX_TABLE[(b & 0x0f) as usize];
    }
    String::from_utf8(buf).expect("hex 编码只产生有效 ASCII 字符")
}

/// 事件广播可观测性验证测试
///
/// 验证 `DownloadEvent` 可通过 `tokio::sync::broadcast` 通道广播并被多个接收者正确消费。
#[cfg(test)]
#[tokio::test]
async fn event_broadcast() {
    use event::DownloadEvent;
    use types::DownloadState;

    // 创建 broadcast 通道，容量 16
    let (tx, _rx) = tokio::sync::broadcast::channel::<DownloadEvent>(16);

    // 构造状态变更事件
    let task_id = TaskId::new_v4();
    let event = DownloadEvent::StateChanged {
        task_id,
        old_state: DownloadState::Pending,
        new_state: DownloadState::Downloading,
    };

    // 订阅两个接收者
    let mut rx1 = tx.subscribe();
    let mut rx2 = tx.subscribe();

    // 广播事件
    tx.send(event.clone()).expect("广播发送不应失败");

    // 验证两个接收者都收到了正确的事件
    let received1 = rx1.recv().await.expect("接收者 1 应收到事件");
    let received2 = rx2.recv().await.expect("接收者 2 应收到事件");

    assert_eq!(received1, event);
    assert_eq!(received2, event);

    // 再广播一个进度事件，验证事件流连续性
    let progress_event = DownloadEvent::Progress {
        task_id,
        downloaded: 1024,
        total: Some(4096),
        speed: 512_000,
    };
    tx.send(progress_event.clone()).expect("广播发送不应失败");

    let received_progress = rx1.recv().await.expect("接收者 1 应收到进度事件");
    assert_eq!(received_progress, progress_event);
}

/// 验证 Metrics 计数器的基本功能
#[cfg(test)]
#[test]
fn metrics() {
    let m = Metrics::new();

    // 初始状态全部为零
    assert_eq!(m.bytes_downloaded.load(Ordering::Relaxed), 0);
    assert_eq!(m.fragments_completed.load(Ordering::Relaxed), 0);
    assert_eq!(m.errors.load(Ordering::Relaxed), 0);

    // 累加字节数
    m.add_bytes(1024);
    m.add_bytes(2048);
    assert_eq!(m.bytes_downloaded.load(Ordering::Relaxed), 3072);

    // 递增分片计数
    m.inc_fragment();
    m.inc_fragment();
    m.inc_fragment();
    assert_eq!(m.fragments_completed.load(Ordering::Relaxed), 3);

    // 递增错误计数
    m.inc_error();
    assert_eq!(m.errors.load(Ordering::Relaxed), 1);

    // Default trait 实现与 new() 等价
    let m2 = Metrics::default();
    assert_eq!(m2.bytes_downloaded.load(Ordering::Relaxed), 0);
}

/// 验证统一配置类型存在且序列化往返正确
#[cfg(test)]
#[test]
fn app_config() {
    let cfg = config::DownloadConfig {
        download_dir: "/tmp/test".to_string(),
        max_concurrent_fragments: 8,
        max_retries: 5,
        request_timeout_secs: 60,
        connect_timeout_secs: 10,
        verify_checksum: false,
        user_agent: "Tachyon/Test".to_string(),
        headers: std::collections::HashMap::new(),
        pause_timeout_secs: 300,
        rate_limit_bytes_per_sec: None,
        authorized_dirs: vec!["/tmp/test".to_string()],
    };
    assert_eq!(cfg.download_dir, "/tmp/test");
    assert_eq!(cfg.max_concurrent_fragments, 8);
    assert_eq!(cfg.max_retries, 5);
    assert_eq!(cfg.request_timeout_secs, 60);
    assert!(!cfg.verify_checksum);
    assert_eq!(cfg.user_agent, "Tachyon/Test");
    assert!(cfg.headers.is_empty());

    // 序列化往返
    let json = serde_json::to_string(&cfg).unwrap();
    assert!(json.contains("downloadDir"), "JSON 应包含字段名: {json}");
    let restored: config::DownloadConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.download_dir, cfg.download_dir);
    assert_eq!(
        restored.max_concurrent_fragments,
        cfg.max_concurrent_fragments
    );
    assert_eq!(restored.max_retries, cfg.max_retries);
    assert_eq!(restored.request_timeout_secs, cfg.request_timeout_secs);
    assert_eq!(restored.verify_checksum, cfg.verify_checksum);
    assert_eq!(restored.user_agent, cfg.user_agent);
}
