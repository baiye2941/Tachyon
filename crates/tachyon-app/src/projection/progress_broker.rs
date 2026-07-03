//! 进度事件代理
//!
//! 将后端任务进度状态投影为前端可消费的 ProgressEvent。
//! 职责：
//! - 全局 progress aggregator：事件驱动 + 250ms 超时兜底扫描
//! - ChunkReaderPool 通过 mark_dirty + Notify 唤醒 aggregator
//! - 合并后发送单个 ProgressEvent，替代每个任务独立的 500ms monitor

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dashmap::{DashMap, DashSet};
use tokio::sync::Notify;

use tokio::sync::watch;

use crate::commands::{ProgressEvent, TaskProgress};
use crate::repository::TaskRepository;

/// 聚合扫描间隔（毫秒）
const AGGREGATOR_INTERVAL_MS: u64 = 250;

/// 进度事件代理
///
/// 全局 progress aggregator：
/// - 事件驱动：ChunkReaderPool 通过 mark_dirty + Notify 唤醒 aggregator
/// - 250ms 超时兜底：确保无通知时也能更新
/// - 合并后发送单个 ProgressEvent，替代每个任务独立的 500ms monitor
pub struct ProgressBroker {
    progress_tx: watch::Sender<ProgressEvent>,
    /// 需要聚合的任务列表引用
    task_repository: TaskRepository,
    /// aggregator 是否已 spawn（幂等防护）
    aggregator_spawned: AtomicBool,
    /// Dirty task IDs — set by ChunkReaderPool when progress changes
    dirty_tasks: Arc<DashSet<String>>,
    /// Notify to wake aggregator
    notify: Arc<Notify>,
    /// 每任务本周期新完成分片索引增量
    pub(crate) pending_deltas: Arc<DashMap<String, Vec<u32>>>,
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
            aggregator_spawned: AtomicBool::new(false),
            dirty_tasks: Arc::new(DashSet::new()),
            notify: Arc::new(Notify::new()),
            pending_deltas: Arc::new(DashMap::new()),
        }
    }

    /// 启动全局 event-driven aggregator
    ///
    /// **必须在 Tokio reactor 上下文中调用**（如 Tauri `setup` 钩子内）。
    /// aggregator 由 ChunkReaderPool 的 mark_dirty 通知唤醒，辅以 250ms 超时兜底。
    /// 幂等：多次调用只启动一个 aggregator（通过 AtomicBool 防重复）。
    pub fn spawn_aggregator(&self) {
        if self.aggregator_spawned.swap(true, Ordering::AcqRel) {
            return;
        }
        let tx = self.progress_tx.clone();
        let task_repository_ref = self.task_repository.clone();
        let dirty_tasks = self.dirty_tasks.clone();
        let notify = self.notify.clone();
        let pending_deltas = self.pending_deltas.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(AGGREGATOR_INTERVAL_MS));
            // Force first tick to fire immediately
            interval.tick().await;

            loop {
                // Wait for either: dirty notification OR interval timeout
                tokio::select! {
                    _ = notify.notified() => {
                        // Debounce: wait a tiny bit for more events to coalesce
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                    _ = interval.tick() => {
                        // Timeout: ensure progress updates even during quiet periods
                    }
                }

                // 直接构建并发送全量事件:进度字段(downloaded/speed/progress/fragments_done)
                // 通过 DashMap 的 get_mut 直接写入,不会触发 TaskRepository::version() 递增,
                // 因此不能用 version 做短路,否则下载期间的进度更新永远无法广播。
                // 下游 compute_progress_delta 会按值过滤掉无变化任务,保证前端不会收到冗余数据。
                let event = build_progress_event(&task_repository_ref, &pending_deltas);
                let _ = tx.send(event);

                // Clear dirty set after building event
                dirty_tasks.clear();
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
            aggregator_spawned: AtomicBool::new(false),
            dirty_tasks: Arc::new(DashSet::new()),
            notify: Arc::new(Notify::new()),
            pending_deltas: Arc::new(DashMap::new()),
        }
    }

    /// Mark a task as having changed progress data.
    /// Called by ChunkReaderPool after updating TaskRepository.
    pub fn mark_dirty(&self, task_id: &str) {
        self.dirty_tasks.insert(task_id.to_string());
        self.notify.notify_one();
    }

    /// 标记任务进度变化,并记录新完成的分片索引
    pub fn mark_dirty_with_delta(&self, task_id: &str, delta_idx: Option<u32>) {
        if let Some(idx) = delta_idx {
            self.pending_deltas
                .entry(task_id.to_string())
                .or_default()
                .push(idx);
        }
        self.dirty_tasks.insert(task_id.to_string());
        self.notify.notify_one();
    }

    /// 广播进度事件（手动触发，用于终态等特殊时刻）
    ///
    /// 扫描当前所有任务状态，构建全量 ProgressEvent 并立即发送。
    /// 不依赖 aggregator 定时器，确保终态变更被即时传播。
    pub fn broadcast_all(&self) {
        let event = build_progress_event(&self.task_repository, &self.pending_deltas);
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
fn build_progress_event(
    task_repository: &TaskRepository,
    pending_deltas: &DashMap<String, Vec<u32>>,
) -> ProgressEvent {
    task_repository
        .iter()
        .map(|r| {
            let id = r.key();
            let t = r.value();
            let completed_delta = pending_deltas
                .get_mut(id)
                .map(|mut d| std::mem::take(&mut *d))
                .unwrap_or_default();
            (
                id.clone(),
                TaskProgress {
                    id: id.clone(),
                    progress: t.progress,
                    speed: t.speed,
                    downloaded: t.downloaded,
                    status: t.status,
                    fragments_done: t.fragments_done,
                    fragments_total: t.fragments_total,
                    active_concurrency: t.active_concurrency,
                    file_size: t.file_size,
                    completed_delta,
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
        let deltas = DashMap::new();
        let event = build_progress_event(&repository, &deltas);
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
                active_concurrency: 0,
                created_at: "2025-01-01T00:00:00+08:00".to_string(),
                save_path: String::new(),
                error_reason: None,
                retry_count: 0,
                hf_meta: None,
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
                active_concurrency: 0,
                created_at: "2025-01-01T00:00:00+08:00".to_string(),
                save_path: String::new(),
                error_reason: None,
                retry_count: 0,
                hf_meta: None,
            },
        );

        let event = build_progress_event(&repository, &DashMap::new());
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
                active_concurrency: 0,
                created_at: "2025-01-01T00:00:00+08:00".to_string(),
                save_path: String::new(),
                error_reason: None,
                retry_count: 0,
                hf_meta: None,
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
                active_concurrency: 0,
                created_at: "2025-01-01T00:00:00+08:00".to_string(),
                save_path: String::new(),
                error_reason: None,
                retry_count: 0,
                hf_meta: None,
            },
        );

        // 应在 AGGREGATOR_INTERVAL_MS 内收到事件
        let result = tokio::time::timeout(Duration::from_millis(500), rx.changed()).await;
        assert!(result.is_ok(), "aggregator 应在 500ms 内发送事件");
    }

    #[tokio::test]
    async fn test_aggregator_broadcasts_progress_field_updates_via_get_mut() {
        // 回归测试:确保 aggregator 不再依赖 version 短路。
        // 进度字段(downloaded/speed/progress/fragments_done)通过 DashMap 的 get_mut
        // 直接写入,不会触发 TaskRepository::version() 递增。aggregator 必须仍能广播,
        // 否则下载期间前端进度永远不更新,直到任务终态(update_status)才显示完成。
        let repository = make_test_repository();
        let broker = ProgressBroker::start(repository.clone());
        broker.spawn_aggregator();
        let mut rx = broker.subscribe();

        repository.insert(
            "t1".to_string(),
            TaskInfo {
                id: "t1".to_string(),
                url: "https://example.com/a.bin".to_string(),
                file_name: "a.bin".to_string(),
                file_size: Some(1024),
                downloaded: 0,
                speed: 0,
                status: DownloadState::Downloading,
                progress: 0.0,
                fragments_total: 4,
                fragments_done: 0,
                active_concurrency: 0,
                created_at: "2025-01-01T00:00:00+08:00".to_string(),
                save_path: String::new(),
                error_reason: None,
                retry_count: 0,
                hf_meta: None,
            },
        );

        // 消费 insert 触发的初始广播
        let _ = tokio::time::timeout(Duration::from_millis(500), rx.changed()).await;
        let _ = rx.borrow_and_update();
        let version_before = repository.version();

        // 模拟 chunk_reader_pool 的进度更新路径:get_mut 改字段,不调 update_status
        if let Some(mut task) = repository.get_mut("t1") {
            task.downloaded = 512;
            task.progress = 0.5;
            task.speed = 100;
            task.fragments_done = 2;
        }
        // 关键不变量:进度字段写入不应递增 version
        assert_eq!(
            repository.version(),
            version_before,
            "get_mut 修改进度字段不应递增 version"
        );

        // aggregator 仍必须在下一个 tick 广播出新值
        let result = tokio::time::timeout(Duration::from_millis(500), rx.changed()).await;
        assert!(
            result.is_ok(),
            "进度字段通过 get_mut 更新后,aggregator 必须广播(不能依赖 version 短路)"
        );
        let snapshot = rx.borrow_and_update().clone();
        let tp = snapshot.get("t1").expect("t1 应在快照中");
        assert_eq!(tp.downloaded, 512);
        assert_eq!(tp.speed, 100);
        assert!((tp.progress - 0.5).abs() < f64::EPSILON);
        assert_eq!(tp.fragments_done, 2);
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
                active_concurrency: 0,
                created_at: "2025-01-01T00:00:00+08:00".to_string(),
                save_path: String::new(),
                error_reason: None,
                retry_count: 0,
                hf_meta: None,
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
