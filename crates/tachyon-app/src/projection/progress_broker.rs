//! 进度事件代理
//!
//! 将后端任务进度状态投影为前端可消费的 ProgressEvent。
//! 职责：
//! - 全局 progress aggregator：事件驱动 + 250ms 无脏通知时的兜底 tick
//! - ChunkReaderPool 通过 mark_dirty + Notify 唤醒 aggregator
//! - 合并后发送单个 ProgressEvent，替代每个任务独立的 500ms monitor
//! - 订阅侧 Lagged 时由 `build_lagged_resync_event` 合成权威全量，保证 delta 最终一致

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use serde::Serialize;
use tokio::sync::Notify;

use tokio::sync::broadcast;

use crate::commands::{FragmentByteProgress, ProgressEvent, TaskProgress};
use crate::repository::TaskRepository;
use crate::runtime::chunk_reader_pool::ProgressDelta;
use tachyon_core::types::DownloadState;

/// 审计 M-03:broadcast 容量;过小会在慢订阅者下 lag 丢事件。
/// 64 在多分片高频 tick 下易触发 Lagged；抬到 256 降低 resync 频率。
/// 即便仍 Lagged，`subscribe_progress` 也会合成权威全量 resync 保证最终一致。
const PROGRESS_BROADCAST_CAPACITY: usize = 256;

/// 任务终态通知 payload
///
/// 与前端 `useTaskNotifications` 约定字段:taskId/title/body/type。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskNotificationPayload {
    pub task_id: String,
    pub title: String,
    pub body: String,
    #[serde(rename = "type")]
    pub notification_type: NotificationType,
}

/// 任务终态通知类型
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationType {
    Completed,
    Failed,
}

/// 任务终态通知发射器抽象
///
/// 生产环境使用 `tauri::AppHandle` 向前端推送 `task-notification` 事件;
/// 测试环境可注入通道/向量实现以捕获事件。
pub trait NotificationEmitter: Send + Sync + 'static {
    fn emit_task_notification(&self, payload: TaskNotificationPayload);
}

impl NotificationEmitter for tauri::AppHandle {
    fn emit_task_notification(&self, payload: TaskNotificationPayload) {
        use tauri::Emitter;
        let _ = self.emit("task-notification", &payload);
    }
}

/// 无脏通知时的兜底 tick 间隔（毫秒）。
///
/// **不是**最小发送间隔：有 `mark_dirty`/`Notify` 时会立即唤醒聚合；
/// 本值仅在安静期保证进度字段仍能刷新。Lagged 恢复不依赖该间隔，
/// 由 `subscribe_progress` 的权威 resync 保证 delta 最终一致。
const AGGREGATOR_INTERVAL_MS: u64 = 250;

/// 进度事件代理
///
/// 全局 progress aggregator：
/// - 事件驱动：ChunkReaderPool 通过 mark_dirty + Notify 唤醒 aggregator
/// - 250ms 兜底 tick：无脏通知时仍刷新进度字段（非最小发送间隔）
/// - 合并后发送单个 ProgressEvent，替代每个任务独立的 500ms monitor
/// - 订阅侧 Lagged 时合成权威 resync，保证分片 delta 最终一致
pub struct ProgressBroker {
    progress_tx: broadcast::Sender<ProgressEvent>,
    /// 需要聚合的任务列表引用
    task_repository: TaskRepository,
    /// aggregator 是否已 spawn（幂等防护）
    aggregator_spawned: AtomicBool,
    /// Notify to wake aggregator
    notify: Arc<Notify>,
    /// 每任务本周期新完成分片索引增量
    pub(crate) pending_completed: Arc<DashMap<String, Vec<u32>>>,
    /// 每任务本周期新开始下载分片索引增量
    pub(crate) pending_started: Arc<DashMap<String, Vec<u32>>>,
    /// 每任务本周期活跃分片字节进度快照(仅 downloading_set 中的分片)
    pub(crate) pending_fragment_bytes: Arc<DashMap<String, Vec<FragmentByteProgress>>>,
    /// 任务终态通知发射器(在 Tauri setup 中注入)
    notification_emitter: Arc<Mutex<Option<Arc<dyn NotificationEmitter>>>>,
    /// 已发送通知的任务终态,用于同一任务同一终态去重
    notified_states: Arc<DashMap<String, DownloadState>>,
}

impl ProgressBroker {
    /// 创建新的 ProgressBroker（不启动 aggregator）
    ///
    /// 构造期间不 spawn 定时器，因为构造可能发生在 Tokio reactor
    /// 尚未就绪的上下文（如 Tauri Builder::manage 同步阶段）。
    /// 生产环境应在 Tauri `setup` 钩子中调用 `spawn_aggregator()`。
    pub fn start(task_repository: TaskRepository) -> Self {
        let (progress_tx, _) = broadcast::channel(PROGRESS_BROADCAST_CAPACITY);
        Self {
            progress_tx,
            task_repository,
            aggregator_spawned: AtomicBool::new(false),
            notify: Arc::new(Notify::new()),
            pending_completed: Arc::new(DashMap::new()),
            pending_started: Arc::new(DashMap::new()),
            pending_fragment_bytes: Arc::new(DashMap::new()),
            notification_emitter: Arc::new(Mutex::new(None)),
            notified_states: Arc::new(DashMap::new()),
        }
    }

    /// 启动全局 event-driven aggregator
    ///
    /// **必须在 Tokio reactor 上下文中调用**（如 Tauri `setup` 钩子内）。
    /// aggregator 由 ChunkReaderPool 的 mark_dirty 通知唤醒，辅以 250ms 无脏通知兜底 tick
    ///（非最小发送间隔；Lagged 恢复见 `subscribe_progress` 权威 resync）。
    /// 幂等：多次调用只启动一个 aggregator（通过 AtomicBool 防重复）。
    pub fn spawn_aggregator(&self) {
        if self.aggregator_spawned.swap(true, Ordering::AcqRel) {
            return;
        }
        let tx = self.progress_tx.clone();
        let task_repository_ref = self.task_repository.clone();
        let notify = self.notify.clone();
        let pending_completed = self.pending_completed.clone();
        let pending_started = self.pending_started.clone();
        let pending_fragment_bytes = self.pending_fragment_bytes.clone();
        let notification_emitter = self.notification_emitter.clone();
        let notified_states = self.notified_states.clone();

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
                let event = build_progress_event(
                    &task_repository_ref,
                    &pending_completed,
                    &pending_started,
                    &pending_fragment_bytes,
                );
                let _ = tx.send(event);

                // 扫描终态任务并触发 `task-notification` 事件(去重)
                emit_terminal_notifications(
                    &task_repository_ref,
                    &notified_states,
                    &notification_emitter,
                );
            }
        });
    }

    /// 创建不启动 aggregator 的 ProgressBroker
    ///
    /// 仅用于测试环境，避免在测试中 spawn 长期运行的定时器。
    pub fn new_no_aggregator(task_repository: TaskRepository) -> Self {
        let (progress_tx, _) = broadcast::channel(PROGRESS_BROADCAST_CAPACITY);
        Self {
            progress_tx,
            task_repository,
            aggregator_spawned: AtomicBool::new(false),
            notify: Arc::new(Notify::new()),
            pending_completed: Arc::new(DashMap::new()),
            pending_started: Arc::new(DashMap::new()),
            pending_fragment_bytes: Arc::new(DashMap::new()),
            notification_emitter: Arc::new(Mutex::new(None)),
            notified_states: Arc::new(DashMap::new()),
        }
    }

    /// 注入任务终态通知发射器并在注入时完成已存在终态的去重种子
    ///
    /// 在 Tauri `setup` 钩子中调用,避免启动前已 Completed/Failed 的任务触发旧通知。
    pub fn set_notification_emitter(&self, emitter: Arc<dyn NotificationEmitter>) {
        // 预填充当前已处于终态的任务,防止启动时广播旧通知
        for r in self.task_repository.iter() {
            let status = r.value().status;
            if matches!(status, DownloadState::Completed | DownloadState::Failed) {
                self.notified_states.insert(r.key().clone(), status);
            }
        }
        *self
            .notification_emitter
            .lock()
            .expect("notification_emitter 锁不应中毒") = Some(emitter);
    }

    /// 唤醒 aggregator(无 delta,仅通知有变化)
    pub fn mark_dirty(&self, _task_id: &str) {
        self.notify.notify_one();
    }

    /// 标记任务进度变化,记录分片状态变更增量(started/completed)+ 活跃分片字节快照
    ///
    /// 竞态消除:当 Completed(idx) 到达时,从 pending_started 中移除 idx(若存在),
    /// 避免同一分片的 Started 增量在跨窗口场景下被推送给前端导致"幽灵 downloading"。
    ///
    /// fragment_bytes 为快照式覆盖:每次调用覆盖该任务本周期的字节快照。
    /// 空 Vec 表示无活跃分片(终态或全部完成)。
    /// "已完成分片不出现在快照"由 broker 层强制:Completed(idx) 到达时即使生产者
    /// 传入的快照滞后仍含 idx,也在此过滤,不只依赖生产者的 frag_bytes.remove。
    pub fn mark_dirty_with_delta(
        &self,
        task_id: &str,
        delta: Option<ProgressDelta>,
        mut fragment_bytes: Vec<FragmentByteProgress>,
    ) {
        if let Some(d) = delta {
            match d {
                ProgressDelta::Started(idx) => {
                    self.pending_started
                        .entry(task_id.to_string())
                        .or_default()
                        .push(idx);
                }
                ProgressDelta::Completed(idx) => {
                    // 后端侧竞态消除:从 pending_started 移除 idx(若存在)
                    if let Some(mut started) = self.pending_started.get_mut(task_id) {
                        started.retain(|&x| x != idx);
                    }
                    // broker 层强制:完成的分片从传入快照中过滤(生产者快照可能滞后)
                    fragment_bytes.retain(|e| e.index != idx);
                    self.pending_completed
                        .entry(task_id.to_string())
                        .or_default()
                        .push(idx);
                }
            }
        }
        // 排序稳定:生产者 frag_bytes 是 HashMap,迭代序不稳定;
        // compute_progress_delta 靠 TaskProgress 整体 PartialEq,
        // 同内容不同序会被误判为变化,每 250ms 触发冗余推送
        fragment_bytes.sort_unstable_by_key(|e| e.index);
        // 字节快照覆盖式写入(空 Vec 也写入,表示清空)
        self.pending_fragment_bytes
            .insert(task_id.to_string(), fragment_bytes);
        self.notify.notify_one();
    }

    /// 广播进度事件（手动触发，用于终态等特殊时刻）
    ///
    /// 扫描当前所有任务状态，构建全量 ProgressEvent 并立即发送。
    /// 不依赖 aggregator 定时器，确保终态变更被即时传播。
    pub fn broadcast_all(&self) {
        let event = build_progress_event(
            &self.task_repository,
            &self.pending_completed,
            &self.pending_started,
            &self.pending_fragment_bytes,
        );
        let _ = self.progress_tx.send(event);

        // 终态特殊时刻同步触发通知,避免等待 aggregator 下一个 tick
        emit_terminal_notifications(
            &self.task_repository,
            &self.notified_states,
            &self.notification_emitter,
        );
    }

    /// 获取订阅 receiver
    ///
    /// 供 `subscribe_progress` Tauri command 使用。
    pub fn subscribe(&self) -> broadcast::Receiver<ProgressEvent> {
        self.progress_tx.subscribe()
    }

    /// 获取 sender 的引用（用于内部传播）
    pub fn sender(&self) -> &broadcast::Sender<ProgressEvent> {
        &self.progress_tx
    }
}

/// 根据任务列表构建全量进度事件
fn build_progress_event(
    task_repository: &TaskRepository,
    pending_completed: &DashMap<String, Vec<u32>>,
    pending_started: &DashMap<String, Vec<u32>>,
    pending_fragment_bytes: &DashMap<String, Vec<FragmentByteProgress>>,
) -> ProgressEvent {
    task_repository
        .iter()
        .map(|r| {
            let id = r.key();
            let t = r.value();
            let completed_delta = pending_completed
                .get_mut(id)
                .map(|mut d| std::mem::take(&mut *d))
                .unwrap_or_default();
            let started_delta = pending_started
                .get_mut(id)
                .map(|mut d| std::mem::take(&mut *d))
                .unwrap_or_default();
            let fragment_bytes = pending_fragment_bytes
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
                    started_delta,
                    error_reason: t.error_reason.clone(),
                    fragment_bytes,
                },
            )
        })
        .collect()
}

/// 扫描任务列表,为首次进入 Completed/Failed 终态的任务发送通知
///
/// - 同一任务同一终态只通知一次(`notified_states` 去重)。
/// - 发射器未注入时(测试/初始化阶段)静默跳过。
fn emit_terminal_notifications(
    task_repository: &TaskRepository,
    notified_states: &DashMap<String, DownloadState>,
    notification_emitter: &Mutex<Option<Arc<dyn NotificationEmitter>>>,
) {
    let emitter = match notification_emitter.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => return,
    };
    let Some(emitter) = emitter else {
        return;
    };

    for r in task_repository.iter() {
        let task_id = r.key();
        let task = r.value();
        let status = task.status;
        if !matches!(status, DownloadState::Completed | DownloadState::Failed) {
            continue;
        }
        // 去重:同一任务同一终态只通知一次
        if notified_states.get(task_id).is_some_and(|s| *s == status) {
            continue;
        }
        notified_states.insert(task_id.clone(), status);

        let (title, body, notification_type) = match status {
            DownloadState::Completed => (
                format!("下载完成: {}", task.file_name),
                format!("{} 已下载完成", task.file_name),
                NotificationType::Completed,
            ),
            DownloadState::Failed => (
                format!("下载失败: {}", task.file_name),
                task.error_reason
                    .clone()
                    .unwrap_or_else(|| format!("{} 下载失败", task.file_name)),
                NotificationType::Failed,
            ),
            _ => continue,
        };

        emitter.emit_task_notification(TaskNotificationPayload {
            task_id: task_id.clone(),
            title,
            body,
            notification_type,
        });
    }
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
        let completed = DashMap::new();
        let started = DashMap::new();
        let event = build_progress_event(&repository, &completed, &started, &DashMap::new());
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
                tags: vec![],
                hf_meta: None,
                display_order: 0,
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
                tags: vec![],
                hf_meta: None,
                display_order: 0,
            },
        );

        let event = build_progress_event(
            &repository,
            &DashMap::new(),
            &DashMap::new(),
            &DashMap::new(),
        );
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
                tags: vec![],
                hf_meta: None,
                display_order: 0,
            },
        );

        broker.broadcast_all();
        let snapshot = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("应收到 broadcast")
            .expect("broadcast 不应关闭");
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
                r = rx.recv() => r.is_ok(),
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
                tags: vec![],
                hf_meta: None,
                display_order: 0,
            },
        );

        // 应在 AGGREGATOR_INTERVAL_MS 内收到事件
        let result = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
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
                tags: vec![],
                hf_meta: None,
                display_order: 0,
            },
        );

        // 消费 insert 触发的初始广播
        let _ = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
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
        let result = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
        assert!(
            result.is_ok(),
            "进度字段通过 get_mut 更新后,aggregator 必须广播(不能依赖 version 短路)"
        );
        let snapshot = result.unwrap().expect("broadcast 不应关闭");
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
                tags: vec![],
                hf_meta: None,
                display_order: 0,
            },
        );

        // 应仍能收到事件(证明至少一个 aggregator 在运行)
        let result = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
        assert!(result.is_ok(), "幂等 spawn 后应仍有 aggregator 运行");

        // 验证标志位已置位
        assert!(
            broker
                .aggregator_spawned
                .load(std::sync::atomic::Ordering::Acquire),
            "aggregator_spawned 标志应为 true"
        );
    }

    /// 测试用通知发射器,通过标准通道捕获事件
    #[derive(Clone)]
    struct TestEmitter {
        tx: std::sync::mpsc::Sender<TaskNotificationPayload>,
    }

    impl NotificationEmitter for TestEmitter {
        fn emit_task_notification(&self, payload: TaskNotificationPayload) {
            let _ = self.tx.send(payload);
        }
    }

    fn make_test_emitter() -> (
        TestEmitter,
        std::sync::mpsc::Receiver<TaskNotificationPayload>,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        (TestEmitter { tx }, rx)
    }

    fn make_completed_task(id: &str, file_name: &str) -> TaskInfo {
        TaskInfo {
            id: id.to_string(),
            url: "https://example.com/a.bin".to_string(),
            file_name: file_name.to_string(),
            file_size: Some(1024),
            downloaded: 1024,
            speed: 0,
            status: DownloadState::Completed,
            progress: 1.0,
            fragments_total: 1,
            fragments_done: 1,
            active_concurrency: 0,
            created_at: "2025-01-01T00:00:00+08:00".to_string(),
            save_path: String::new(),
            error_reason: None,
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
        }
    }

    fn make_failed_task(id: &str, file_name: &str, error_reason: Option<&str>) -> TaskInfo {
        TaskInfo {
            id: id.to_string(),
            url: "https://example.com/a.bin".to_string(),
            file_name: file_name.to_string(),
            file_size: Some(1024),
            downloaded: 512,
            speed: 0,
            status: DownloadState::Failed,
            progress: 0.5,
            fragments_total: 2,
            fragments_done: 1,
            active_concurrency: 0,
            created_at: "2025-01-01T00:00:00+08:00".to_string(),
            save_path: String::new(),
            error_reason: error_reason.map(String::from),
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
        }
    }

    #[test]
    fn test_emit_terminal_notifications_completed() {
        let repository = make_test_repository();
        let broker = ProgressBroker::new_no_aggregator(repository.clone());
        let (emitter, rx) = make_test_emitter();
        broker.set_notification_emitter(Arc::new(emitter));

        repository.insert("t1".to_string(), make_completed_task("t1", "model.gguf"));
        broker.broadcast_all();

        let payload = rx.recv().expect("应收到 Completed 通知");
        assert_eq!(payload.task_id, "t1");
        assert!(matches!(
            payload.notification_type,
            NotificationType::Completed
        ));
        assert_eq!(payload.title, "下载完成: model.gguf");
        assert_eq!(payload.body, "model.gguf 已下载完成");
    }

    #[test]
    fn test_emit_terminal_notifications_failed() {
        let repository = make_test_repository();
        let broker = ProgressBroker::new_no_aggregator(repository.clone());
        let (emitter, rx) = make_test_emitter();
        broker.set_notification_emitter(Arc::new(emitter));

        repository.insert(
            "t2".to_string(),
            make_failed_task("t2", "data.zip", Some("connection reset")),
        );
        broker.broadcast_all();

        let payload = rx.recv().expect("应收到 Failed 通知");
        assert_eq!(payload.task_id, "t2");
        assert!(matches!(
            payload.notification_type,
            NotificationType::Failed
        ));
        assert_eq!(payload.title, "下载失败: data.zip");
        assert_eq!(payload.body, "connection reset");
    }

    #[test]
    fn test_terminal_notification_deduplicated_per_state() {
        let repository = make_test_repository();
        let broker = ProgressBroker::new_no_aggregator(repository.clone());
        let (emitter, rx) = make_test_emitter();
        broker.set_notification_emitter(Arc::new(emitter));

        repository.insert("t3".to_string(), make_completed_task("t3", "file.bin"));
        broker.broadcast_all();
        assert!(rx.recv().is_ok(), "首次 Completed 应触发通知");

        // 再次 broadcast,同一任务同一终态不应重复通知
        broker.broadcast_all();
        assert!(
            rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "同一终态不应重复通知"
        );

        // 状态变为 Failed 后应再次触发(不同终态)
        if let Some(mut task) = repository.get_mut("t3") {
            task.status = DownloadState::Failed;
            task.error_reason = Some("verify failed".to_string());
        }
        broker.broadcast_all();
        let payload = rx.recv().expect("状态变更后应再次通知");
        assert!(matches!(
            payload.notification_type,
            NotificationType::Failed
        ));
    }

    #[test]
    fn test_set_emitter_seeds_existing_terminal_states() {
        let repository = make_test_repository();
        let broker = ProgressBroker::new_no_aggregator(repository.clone());
        repository.insert("t4".to_string(), make_completed_task("t4", "old.bin"));

        let (emitter, rx) = make_test_emitter();
        // 注入 emitter 时应将已存在的 Completed 任务标记为已通知
        broker.set_notification_emitter(Arc::new(emitter));

        broker.broadcast_all();
        assert!(
            rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "注入前已存在的终态任务不应触发旧通知"
        );
    }

    #[test]
    fn test_no_emitter_no_panic() {
        let repository = make_test_repository();
        let broker = ProgressBroker::new_no_aggregator(repository.clone());
        repository.insert("t5".to_string(), make_completed_task("t5", "x.bin"));
        // 未注入 emitter 时 broadcast 不应 panic
        broker.broadcast_all();
    }

    /// 审计 M-03:连续两次 broadcast 带不同 completed_delta,订阅者必须都能收到
    /// (watch 只会保留最后一次,导致第一次 delta 永久丢失)
    #[tokio::test]
    async fn test_m03_consecutive_broadcasts_preserve_completed_deltas() {
        let repository = make_test_repository();
        let broker = ProgressBroker::new_no_aggregator(repository.clone());
        let mut rx = broker.subscribe();

        repository.insert(
            "t-delta".to_string(),
            TaskInfo {
                id: "t-delta".to_string(),
                url: "https://example.com/d.bin".to_string(),
                file_name: "d.bin".to_string(),
                file_size: Some(1024),
                downloaded: 256,
                speed: 10,
                status: DownloadState::Downloading,
                progress: 0.25,
                fragments_total: 4,
                fragments_done: 1,
                active_concurrency: 1,
                created_at: "2025-01-01T00:00:00+08:00".to_string(),
                save_path: String::new(),
                error_reason: None,
                retry_count: 0,
                tags: vec![],
                hf_meta: None,
                display_order: 0,
            },
        );

        broker.mark_dirty_with_delta("t-delta", Some(ProgressDelta::Completed(0)), vec![]);
        broker.broadcast_all();
        broker.mark_dirty_with_delta("t-delta", Some(ProgressDelta::Completed(1)), vec![]);
        broker.broadcast_all();

        let e1 = tokio::time::timeout(Duration::from_millis(300), rx.recv())
            .await
            .expect("e1 timeout")
            .expect("e1 closed");
        let e2 = tokio::time::timeout(Duration::from_millis(300), rx.recv())
            .await
            .expect("e2 timeout")
            .expect("e2 closed");

        let d1 = e1.get("t-delta").unwrap().completed_delta.clone();
        let d2 = e2.get("t-delta").unwrap().completed_delta.clone();
        assert_eq!(
            d1,
            vec![0],
            "第一次 broadcast 的 completed_delta 不得被覆盖丢失"
        );
        assert_eq!(
            d2,
            vec![1],
            "第二次 broadcast 的 completed_delta 必须独立到达"
        );
    }

    /// 字节级进度:mark_dirty_with_delta 携带的 fragment_bytes 应出现在 broadcast 事件中
    #[tokio::test]
    async fn test_fragment_bytes_propagated_to_broadcast() {
        let repository = make_test_repository();
        repository.insert(
            "t-bytes".to_string(),
            TaskInfo {
                id: "t-bytes".to_string(),
                url: "https://example.com/a.bin".to_string(),
                file_name: "a.bin".to_string(),
                file_size: Some(1024),
                downloaded: 0,
                speed: 0,
                status: DownloadState::Downloading,
                progress: 0.0,
                fragments_total: 4,
                fragments_done: 0,
                active_concurrency: 2,
                created_at: "2025-01-01T00:00:00+08:00".to_string(),
                save_path: String::new(),
                error_reason: None,
                retry_count: 0,
                tags: vec![],
                hf_meta: None,
                display_order: 0,
            },
        );
        let broker = ProgressBroker::new_no_aggregator(repository.clone());
        let mut rx = broker.subscribe();

        broker.mark_dirty_with_delta(
            "t-bytes",
            None,
            vec![
                FragmentByteProgress {
                    index: 0,
                    downloaded: 256,
                },
                FragmentByteProgress {
                    index: 1,
                    downloaded: 128,
                },
            ],
        );
        broker.broadcast_all();

        let event = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("应收到 broadcast")
            .expect("broadcast 不应关闭");
        let tp = event.get("t-bytes").expect("t-bytes 应在事件中");
        assert_eq!(
            tp.fragment_bytes.len(),
            2,
            "fragment_bytes 应含 2 个活跃分片"
        );
        let mut entries = tp.fragment_bytes.clone();
        entries.sort_by_key(|e| e.index);
        assert_eq!(entries[0].index, 0);
        assert_eq!(entries[0].downloaded, 256);
        assert_eq!(entries[1].index, 1);
        assert_eq!(entries[1].downloaded, 128);
    }

    /// 终态后 fragment_bytes 应被清空(活跃分片 0 个)
    #[tokio::test]
    async fn test_fragment_bytes_cleared_after_terminal() {
        let repository = make_test_repository();
        repository.insert("t-term".to_string(), make_completed_task("t-term", "a.bin"));
        let broker = ProgressBroker::new_no_aggregator(repository.clone());
        let mut rx = broker.subscribe();

        broker.mark_dirty_with_delta(
            "t-term",
            None,
            vec![FragmentByteProgress {
                index: 0,
                downloaded: 256,
            }],
        );
        broker.mark_dirty_with_delta("t-term", None, vec![]);
        broker.broadcast_all();

        let event = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("应收到 broadcast")
            .expect("broadcast 不应关闭");
        let tp = event.get("t-term").expect("t-term 应在事件中");
        assert!(tp.fragment_bytes.is_empty(), "终态后 fragment_bytes 应为空");
    }

    /// Completed delta 到达时,即使传入快照滞后含该 idx,broker 也应过滤掉
    #[tokio::test]
    async fn test_fragment_bytes_completed_index_filtered() {
        let repository = make_test_repository();
        repository.insert(
            "t-x".to_string(),
            TaskInfo {
                id: "t-x".to_string(),
                url: "https://example.com/a.bin".to_string(),
                file_name: "a.bin".to_string(),
                file_size: Some(1024),
                downloaded: 384,
                speed: 100,
                status: DownloadState::Downloading,
                progress: 0.375,
                fragments_total: 4,
                fragments_done: 1,
                active_concurrency: 1,
                created_at: "2025-01-01T00:00:00+08:00".to_string(),
                save_path: String::new(),
                error_reason: None,
                retry_count: 0,
                tags: vec![],
                hf_meta: None,
                display_order: 0,
            },
        );
        let broker = ProgressBroker::new_no_aggregator(repository.clone());
        let mut rx = broker.subscribe();

        // 生产者快照滞后:Completed(1) 到达时快照仍含 index=1
        broker.mark_dirty_with_delta(
            "t-x",
            Some(ProgressDelta::Completed(1)),
            vec![
                FragmentByteProgress {
                    index: 0,
                    downloaded: 256,
                },
                FragmentByteProgress {
                    index: 1,
                    downloaded: 128,
                },
            ],
        );
        broker.broadcast_all();

        let event = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("应收到 broadcast")
            .expect("broadcast 不应关闭");
        let tp = event.get("t-x").expect("t-x 应在事件中");
        assert_eq!(
            tp.fragment_bytes.len(),
            1,
            "已完成分片 index=1 不应出现在字节快照中"
        );
        assert_eq!(tp.fragment_bytes[0].index, 0);
        assert_eq!(tp.fragment_bytes[0].downloaded, 256);
    }

    /// 快照顺序稳定:同集合不同序传入,两次 broadcast 的 TaskProgress 应相等,
    /// 否则 compute_progress_delta 会每 250ms 误判变化触发冗余推送
    #[tokio::test]
    async fn test_fragment_bytes_order_stable_for_delta() {
        let repository = make_test_repository();
        repository.insert(
            "t-ord".to_string(),
            TaskInfo {
                id: "t-ord".to_string(),
                url: "https://example.com/a.bin".to_string(),
                file_name: "a.bin".to_string(),
                file_size: Some(1024),
                downloaded: 384,
                speed: 100,
                status: DownloadState::Downloading,
                progress: 0.375,
                fragments_total: 4,
                fragments_done: 0,
                active_concurrency: 2,
                created_at: "2025-01-01T00:00:00+08:00".to_string(),
                save_path: String::new(),
                error_reason: None,
                retry_count: 0,
                tags: vec![],
                hf_meta: None,
                display_order: 0,
            },
        );
        let broker = ProgressBroker::new_no_aggregator(repository.clone());
        let mut rx = broker.subscribe();

        broker.mark_dirty_with_delta(
            "t-ord",
            None,
            vec![
                FragmentByteProgress {
                    index: 0,
                    downloaded: 256,
                },
                FragmentByteProgress {
                    index: 1,
                    downloaded: 128,
                },
            ],
        );
        broker.broadcast_all();
        // 同集合不同序(HashMap 迭代序不稳定的模拟)
        broker.mark_dirty_with_delta(
            "t-ord",
            None,
            vec![
                FragmentByteProgress {
                    index: 1,
                    downloaded: 128,
                },
                FragmentByteProgress {
                    index: 0,
                    downloaded: 256,
                },
            ],
        );
        broker.broadcast_all();

        let e1 = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("e1 应收到")
            .expect("e1 不应关闭");
        let e2 = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("e2 应收到")
            .expect("e2 不应关闭");
        assert_eq!(
            e1.get("t-ord"),
            e2.get("t-ord"),
            "同集合不同序传入,TaskProgress 应相等(快照需排序稳定)"
        );
    }
}
