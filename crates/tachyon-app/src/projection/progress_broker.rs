//! 进度事件代理
//!
//! 将后端任务进度状态投影为前端可消费的 ProgressEvent。
//! 职责：
//! - 全局 progress aggregator：单一 250ms 定时器扫描所有活跃任务的进度
//! - 合并后发送单个 ProgressEvent，替代每个任务独立的 500ms monitor
//! - 活跃任务数从 O(tasks) events/s 降为 O(1) event/250ms

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::watch;

use crate::commands::{ProgressEvent, TaskProgress};
use crate::repository::TaskRepository;

/// 聚合扫描间隔（毫秒）
const AGGREGATOR_INTERVAL_MS: u64 = 250;

/// 进度事件代理
///
/// 全局 progress aggregator：
/// - 单一 250ms 定时器扫描所有活跃任务的进度
/// - 合并后发送单个 ProgressEvent，替代每个任务独立的 500ms monitor
/// - 活跃任务数从 O(tasks) events/s 降为 O(1) event/250ms
pub struct ProgressBroker {
    progress_tx: watch::Sender<ProgressEvent>,
    /// 需要聚合的任务列表引用
    task_repository: TaskRepository,
    /// aggregator 是否已 spawn（幂等防护）
    aggregator_spawned: std::sync::atomic::AtomicBool,
}

impl ProgressBroker {
    /// 创建新的 ProgressBroker（不启动 aggregator）
    ///
    /// 构造期间不 spawn 定时器，因为构造可能发生在 Tokio reactor
    /// 尚未就绪的上下文（如 Tauri Builder::manage 同步阶段）。
    /// 生产环境应在 Tauri `setup` 钩子中调用 `spawn_aggregator()`。
    pub fn start(task_repository: TaskRepository) -> Self {
        let progress_tx = watch::Sender::new(HashMap::new());
        Self {
            progress_tx,
            task_repository,
            aggregator_spawned: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// 启动全局 aggregator 定时器
    ///
    /// **必须在 Tokio reactor 上下文中调用**（如 Tauri `setup` 钩子内）。
    /// aggregator 以 250ms 间隔定期扫描 tasks，构建全量 ProgressEvent 并发送。
    /// 幂等：多次调用只启动一个 aggregator（通过 AtomicBool 防重复）。
    pub fn spawn_aggregator(&self) {
        // 原子防重复：首次调用置位，后续调用直接返回
        if self
            .aggregator_spawned
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let tx = self.progress_tx.clone();
        let task_repository_ref = self.task_repository.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(AGGREGATOR_INTERVAL_MS));
            let mut last_version: u64 = 0;
            loop {
                interval.tick().await;
                // 仅在任务仓库有变更时才构建进度事件,避免无变化时的全量扫描
                let current_version = task_repository_ref.version();
                if current_version == last_version {
                    continue;
                }
                last_version = current_version;
                let event = build_progress_event(&task_repository_ref);
                let _ = tx.send(event);
            }
        });
    }

    /// 创建不启动 aggregator 的 ProgressBroker
    ///
    /// 仅用于测试环境，避免在测试中 spawn 长期运行的定时器。
    pub fn new_no_aggregator(task_repository: TaskRepository) -> Self {
        let progress_tx = watch::Sender::new(HashMap::new());
        Self {
            progress_tx,
            task_repository,
            aggregator_spawned: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// 广播进度事件（手动触发，用于终态等特殊时刻）
    ///
    /// 扫描当前所有任务状态，构建全量 ProgressEvent 并立即发送。
    /// 不依赖 aggregator 定时器，确保终态变更被即时传播。
    pub fn broadcast_all(&self) {
        let event = build_progress_event(&self.task_repository);
        let _ = self.progress_tx.send(event);
    }

    /// 获取订阅 receiver
    ///
    /// 供 `subscribe_progress` Tauri command 使用。
    pub fn subscribe(&self) -> watch::Receiver<ProgressEvent> {
        self.progress_tx.subscribe()
    }

    /// 获取 sender 的引用（用于内部传播）
    pub fn sender(&self) -> &watch::Sender<ProgressEvent> {
        &self.progress_tx
    }
}

/// 根据任务列表构建全量进度事件
fn build_progress_event(task_repository: &TaskRepository) -> ProgressEvent {
    task_repository
        .iter()
        .map(|r| {
            let id = r.key();
            let t = r.value();
            (
                id.clone(),
                TaskProgress {
                    id: id.clone(),
                    progress: t.progress,
                    speed: t.speed,
                    downloaded: t.downloaded,
                    status: t.status,
                    fragments_done: t.fragments_done,
                },
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::TaskInfo;
    use tachyon_core::types::DownloadState;

    fn make_test_repository() -> TaskRepository {
        TaskRepository::new()
    }

    #[test]
    fn test_build_progress_event_empty() {
        let repository = make_test_repository();
        let event = build_progress_event(&repository);
        assert!(event.is_empty());
    }

    #[test]
    fn test_build_progress_event_with_tasks() {
        let repository = make_test_repository();
        repository.insert(
            "t1".to_string(),
            TaskInfo {
                id: "t1".to_string(),
                url: "https://example.com/a.bin".to_string(),
                file_name: "a.bin".to_string(),
                file_size: Some(1024),
                downloaded: 512,
                speed: 100,
                status: DownloadState::Downloading,
                progress: 0.5,
                fragments_total: 4,
                fragments_done: 2,
                created_at: "2025-01-01T00:00:00+08:00".to_string(),
                save_path: String::new(),
                error_reason: None,
                retry_count: 0,
            },
        );
        repository.insert(
            "t2".to_string(),
            TaskInfo {
                id: "t2".to_string(),
                url: "https://example.com/b.bin".to_string(),
                file_name: "b.bin".to_string(),
                file_size: Some(2048),
                downloaded: 2048,
                speed: 0,
                status: DownloadState::Completed,
                progress: 1.0,
                fragments_total: 2,
                fragments_done: 2,
                created_at: "2025-01-01T00:00:00+08:00".to_string(),
                save_path: String::new(),
                error_reason: None,
                retry_count: 0,
            },
        );

        let event = build_progress_event(&repository);
        assert_eq!(event.len(), 2);

        let tp1 = event.get("t1").unwrap();
        assert!((tp1.progress - 0.5).abs() < f64::EPSILON);
        assert_eq!(tp1.speed, 100);
        assert_eq!(tp1.downloaded, 512);
        assert_eq!(tp1.fragments_done, 2);

        let tp2 = event.get("t2").unwrap();
        assert!((tp2.progress - 1.0).abs() < f64::EPSILON);
        assert_eq!(tp2.speed, 0);
    }

    #[tokio::test]
    async fn test_broadcast_all_sends_event() {
        let repository = make_test_repository();
        let broker = ProgressBroker::new_no_aggregator(repository.clone());
        let mut rx = broker.subscribe();

        repository.insert(
            "t1".to_string(),
            TaskInfo {
                id: "t1".to_string(),
                url: "https://example.com/a.bin".to_string(),
                file_name: "a.bin".to_string(),
                file_size: Some(1024),
                downloaded: 512,
                speed: 100,
                status: DownloadState::Downloading,
                progress: 0.5,
                fragments_total: 4,
                fragments_done: 2,
                created_at: "2025-01-01T00:00:00+08:00".to_string(),
                save_path: String::new(),
                error_reason: None,
                retry_count: 0,
            },
        );

        broker.broadcast_all();
        rx.changed().await.unwrap();
        let snapshot = rx.borrow_and_update().clone();
        assert_eq!(snapshot.len(), 1);
        assert!(snapshot.contains_key("t1"));
    }

    #[test]
    fn test_new_no_aggregator_does_not_spawn_timer() {
        let repository = make_test_repository();
        let broker = ProgressBroker::new_no_aggregator(repository.clone());
        let mut rx = broker.subscribe();

        // 不应收到任何事件（没有定时器驱动）
        // 短暂等待确认不会收到事件
        let result = tokio::runtime::Runtime::new().unwrap().block_on(async {
            tokio::select! {
                _ = rx.changed() => true,
                _ = tokio::time::sleep(Duration::from_millis(100)) => false,
            }
        });
        assert!(!result, "不应在无 aggregator 时收到事件");
    }

    #[tokio::test]
    async fn test_aggregator_sends_periodic_events() {
        let repository = make_test_repository();
        let broker = ProgressBroker::start(repository.clone());
        // 显式启动 aggregator(测试在 #[tokio::test] reactor 上下文中)
        broker.spawn_aggregator();
        let mut rx = broker.subscribe();

        repository.insert(
            "t1".to_string(),
            TaskInfo {
                id: "t1".to_string(),
                url: "https://example.com/a.bin".to_string(),
                file_name: "a.bin".to_string(),
                file_size: Some(1024),
                downloaded: 512,
                speed: 100,
                status: DownloadState::Downloading,
                progress: 0.5,
                fragments_total: 4,
                fragments_done: 2,
                created_at: "2025-01-01T00:00:00+08:00".to_string(),
                save_path: String::new(),
                error_reason: None,
                retry_count: 0,
            },
        );

        // 应在 AGGREGATOR_INTERVAL_MS 内收到事件
        let result = tokio::time::timeout(Duration::from_millis(500), rx.changed()).await;
        assert!(result.is_ok(), "aggregator 应在 500ms 内发送事件");
    }

    #[tokio::test]
    async fn test_spawn_aggregator_is_idempotent() {
        // 多次调用 spawn_aggregator 应只启动一个定时器
        let repository = make_test_repository();
        let broker = ProgressBroker::start(repository.clone());
        let mut rx = broker.subscribe();

        broker.spawn_aggregator();
        broker.spawn_aggregator(); // 第二次应被 AtomicBool 拦截

        repository.insert(
            "t1".to_string(),
            TaskInfo {
                id: "t1".to_string(),
                url: "https://example.com/a.bin".to_string(),
                file_name: "a.bin".to_string(),
                file_size: Some(1024),
                downloaded: 512,
                speed: 100,
                status: DownloadState::Downloading,
                progress: 0.5,
                fragments_total: 4,
                fragments_done: 2,
                created_at: "2025-01-01T00:00:00+08:00".to_string(),
                save_path: String::new(),
                error_reason: None,
                retry_count: 0,
            },
        );

        // 应仍能收到事件(证明至少一个 aggregator 在运行)
        let result = tokio::time::timeout(Duration::from_millis(500), rx.changed()).await;
        assert!(result.is_ok(), "幂等 spawn 后应仍有 aggregator 运行");

        // 验证标志位已置位
        assert!(
            broker
                .aggregator_spawned
                .load(std::sync::atomic::Ordering::Acquire),
            "aggregator_spawned 标志应为 true"
        );
    }
}
