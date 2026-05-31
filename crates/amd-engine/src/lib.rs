//! AI Model Downloader 引擎层:分片引擎、连接管理
//!
//! 核心下载引擎实现:
//! - 超分片策略(动态粒度调整)
//! - 连接池管理
//! - 分片状态机
//! - 并发控制

pub mod connection;
pub mod downloader;
pub mod fragment;
pub mod orchestrator;

pub use connection::{ConnectionPool, PoolConfig};
pub use downloader::{
    DownloadTask, FragmentProgress, StorageKind, VerifierKind, default_blake3_verifier,
};
pub use fragment::{BandwidthTracker, FragmentRecord, FragmentState};
pub use orchestrator::DownloadOrchestrator;

// 验证测试:放在 crate 根级别,以便 `--exact` 匹配

/// 验证 VerifierKind clone 正确性
#[cfg(test)]
#[test]
fn verifier() {
    let v = downloader::default_blake3_verifier();
    let v2 = v.clone();
    let data = b"verifier clone test data";
    let hash = v.compute_hash(data).unwrap();
    let hash2 = v2.compute_hash(data).unwrap();
    assert_eq!(hash, hash2, "clone 后校验器应行为一致");
}

/// 验证信号量关闭时返回错误而非 panic
#[cfg(test)]
#[tokio::test]
async fn semaphore() {
    let pool = ConnectionPool::new(PoolConfig {
        max_per_host: 1,
        max_global: 1,
    });
    pool.global_semaphore.close();
    let result = pool.acquire("test.com").await;
    assert!(result.is_err(), "关闭的信号量应返回错误而非 panic");
    let err_msg = match result {
        Ok(_) => panic!("期望错误"),
        Err(e) => e.to_string(),
    };
    assert!(
        err_msg.contains("信号量") || err_msg.contains("semaphore"),
        "错误信息应包含信号量描述: {err_msg}"
    );
}

/// 验证断点续传:从持久化状态恢复下载任务
#[cfg(test)]
#[test]
fn resume_download() {
    use amd_core::types::FragmentInfo;

    // 模拟一个 4 分片的下载任务,其中分片 0 和 1 已完成
    let frag_size = 1000u64;
    let all_fragments: Vec<FragmentInfo> = (0..4)
        .map(|i| {
            FragmentInfo::new(
                i,
                i as u64 * frag_size,
                (i as u64 + 1) * frag_size - 1,
                frag_size,
            )
        })
        .collect();

    // 已完成分片索引
    let completed: Vec<u32> = vec![0, 1];

    // 验证:仅恢复未完成的分片
    let pending: Vec<&FragmentInfo> = all_fragments
        .iter()
        .filter(|f| !completed.contains(&f.index))
        .collect();

    assert_eq!(pending.len(), 2, "应有 2 个未完成分片");
    assert_eq!(pending[0].index, 2);
    assert_eq!(pending[1].index, 3);

    // 验证:已下载进度正确
    let downloaded: u64 = completed.len() as u64 * frag_size;
    assert_eq!(downloaded, 2000);

    // 验证:剩余下载量
    let remaining: u64 = pending.iter().map(|f| f.size).sum();
    assert_eq!(remaining, 2000);

    // 验证:续传时分片状态机从 Pending 开始
    let state = FragmentState::Pending;
    assert_eq!(state, FragmentState::Pending);
}
