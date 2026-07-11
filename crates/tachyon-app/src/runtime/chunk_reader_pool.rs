//! 共享 ChunkReader 工作池
//!
//! 每个下载任务不再 spawn 独立的 chunk_reader_handle tokio task，
//! 而是通过共享的 ChunkReaderPool 提交进度处理任务。
//! 工作池固定 N 个 worker（N = max_concurrent_tasks），避免随任务数线性增长的 tokio task 数量。
//!
//! 架构: submit → mpsc channel → dispatcher task → per-worker channel → worker tasks
//! 消除原 `Arc<Mutex<Receiver>>` 导致的 worker 串行化问题。

use std::collections::BTreeSet;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use tachyon_core::FragmentProgress;

/// 进度变化增量类型(传给 ProgressBroker,用于区分 started/completed delta)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProgressDelta {
    /// 分片开始下载(Started 事件)
    Started(u32),
    /// 分片完成(Chunk{completed:true} 事件)
    Completed(u32),
}

/// 进度变化回调:参数为 (task_id, delta),None 表示非状态变更事件(增量字节进度)
pub type ProgressCallback = Arc<dyn Fn(&str, Option<ProgressDelta>) + Send + Sync>;

use crate::repository::TaskRepository;
use crate::task_store::TaskStore;

// ---------------------------------------------------------------------------
// ChunkReaderJob: 提交到池的进度处理任务
// ---------------------------------------------------------------------------

/// 提交到 ChunkReaderPool 的进度处理任务
pub struct ChunkReaderJob {
    /// 任务 ID
    pub task_id: String,
    /// 分片进度事件接收端
    pub progress_rx: mpsc::Receiver<FragmentProgress>,
    /// 内存中的任务表
    pub task_repository: TaskRepository,
    /// 任务持久化存储
    pub task_store: Arc<TaskStore>,
    /// 完成通知：当 job 处理完毕后发送信号
    pub done_tx: oneshot::Sender<()>,
    /// Callback to notify ProgressBroker of progress changes
    /// 第二参数: 新完成分片 index; None = 非完成事件(增量进度)
    pub on_progress: Option<ProgressCallback>,
    /// 分片状态存储(PlanComplete/Chunk 事件更新)
    pub fragment_state_store: crate::projection::FragmentStateStore,
}

// ---------------------------------------------------------------------------
// ChunkReaderPool
// ---------------------------------------------------------------------------

/// 共享 ChunkReader 工作池
///
/// 使用 dispatcher + per-worker channel 架构,避免 worker 串行化。
/// submit() 将 job 发送到 mpsc channel,dispatcher 任务 round-robin
/// 分发到 N 个 worker 的专用 channel,每个 worker 独立拉取 job。
pub struct ChunkReaderPool {
    /// 任务提交通道
    job_tx: mpsc::Sender<ChunkReaderJob>,
    /// 中心 receiver（spawn_workers 时消费,交给 dispatcher）
    job_rx: std::sync::Mutex<Option<mpsc::Receiver<ChunkReaderJob>>>,
    /// worker 是否已 spawn（幂等防护）
    workers_spawned: std::sync::atomic::AtomicBool,
    /// 预设 worker 数量
    worker_count: usize,
}

impl ChunkReaderPool {
    /// 创建新的 ChunkReaderPool（不启动 worker）
    ///
    /// 构造期间不 spawn worker，因为构造可能发生在 Tokio reactor
    /// 尚未就绪的上下文（如 Tauri Builder::manage 同步阶段）。
    /// 生产环境应在 Tauri `setup` 钩子中调用 `spawn_workers()`。
    ///
    /// `worker_count` 通常等于 max_concurrent_tasks，确保每个并发下载有一个 worker。
    pub fn new(worker_count: usize) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<ChunkReaderJob>(worker_count * 2);
        Self {
            job_tx,
            job_rx: std::sync::Mutex::new(Some(job_rx)),
            workers_spawned: std::sync::atomic::AtomicBool::new(false),
            worker_count,
        }
    }

    /// 启动 worker 协程
    ///
    /// **必须在 Tokio reactor 上下文中调用**（如 Tauri `setup` 钩子内）。
    /// 幂等：多次调用只启动一组 worker（通过 AtomicBool 防重复）。
    pub fn spawn_workers(&self) {
        if self
            .workers_spawned
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        // spawn_workers 仅在首次调用时执行，此时 job_rx 必定存在
        let mut job_rx = self
            .job_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .expect("spawn_workers: job_rx 不应为 None（仅首次调用时消费）");

        // 创建 per-worker channel（buffer=1 允许 dispatcher 预派发一个 job）
        let mut worker_txs: Vec<mpsc::Sender<ChunkReaderJob>> = Vec::new();
        let mut worker_rxs: Vec<mpsc::Receiver<ChunkReaderJob>> = Vec::new();
        for _ in 0..self.worker_count {
            let (tx, rx) = mpsc::channel::<ChunkReaderJob>(1);
            worker_txs.push(tx);
            worker_rxs.push(rx);
        }

        // dispatcher: 从中心 channel 读取 job,round-robin 分发到 per-worker channel
        tokio::spawn(async move {
            let mut next_worker = 0usize;
            while let Some(job) = job_rx.recv().await {
                let worker_id = next_worker % worker_txs.len();
                if worker_txs[worker_id].send(job).await.is_err() {
                    tracing::debug!(worker_id, "chunk reader worker 已退出,丢弃 job");
                }
                next_worker = worker_id.wrapping_add(1);
            }
            // 中心 channel 关闭,通知所有 worker 退出
            drop(worker_txs);
        });

        // 启动 N 个 worker,每个持有自己的 receiver
        for worker_id in 0..self.worker_count {
            let mut rx = worker_rxs
                .pop()
                .expect("worker_rxs 数量应匹配 worker_count");
            tokio::spawn(async move {
                while let Some(job) = rx.recv().await {
                    run_chunk_reader(job).await;
                }
                tracing::debug!(worker_id, "chunk reader worker 退出:通道已关闭");
            });
        }
    }

    /// 提交进度处理任务到池
    ///
    /// 返回 oneshot Receiver，当进度处理完毕后收到信号。
    pub fn submit(
        &self,
        job: ChunkReaderJob,
    ) -> Result<(), mpsc::error::SendError<ChunkReaderJob>> {
        self.job_tx.blocking_send(job)
    }

    /// 异步提交进度处理任务到池
    pub async fn submit_async(
        &self,
        job: ChunkReaderJob,
    ) -> Result<(), mpsc::error::SendError<ChunkReaderJob>> {
        self.job_tx.send(job).await
    }
}

// ---------------------------------------------------------------------------
// Chunk reader 进度处理逻辑（从 task_commands::task_fn 提取）
// ---------------------------------------------------------------------------

/// 运行 chunk reader 进度处理
///
/// 与原 task_fn 中 spawn 的 chunk_reader_handle 逻辑完全一致，
/// 仅从独立 spawn 改为由 pool dispatch。
async fn run_chunk_reader(job: ChunkReaderJob) {
    let ChunkReaderJob {
        task_id,
        mut progress_rx,
        task_repository,
        task_store,
        done_tx,
        on_progress,
        fragment_state_store,
    } = job;

    // 已完成分片集合,用于断点续传 checkpoint
    let mut completed: BTreeSet<u32> = BTreeSet::new();
    // 从 tasks 读取 probe 阶段已写入的 total_frags(PlanComplete 到达时覆盖为真实值)
    let mut total_frags = task_repository
        .get(&task_id)
        .map(|t| t.fragments_total)
        .unwrap_or(0);
    tracing::info!(task_id = %task_id, total_frags, "chunk reader 启动,等待进度事件");
    // 跟踪每个分片的已下载字节数
    let mut frag_bytes: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    let mut total_downloaded: u64 = 0;
    let mut event_count: u64 = 0;
    // checkpoint 批量合并
    let mut pending_completed: Vec<u32> = Vec::new();
    const CHECKPOINT_BATCH_SIZE: usize = 5;
    // 字节级进度 checkpoint 间隔(事件数)
    const PARTIAL_CHECKPOINT_INTERVAL: u64 = 20;
    let mut partial_checkpoint_counter: u64 = 0;

    // 速度计算状态
    let mut last_speed_sample: u64 = 0;
    let mut last_speed_time = tokio::time::Instant::now();

    while let Some(progress) = progress_rx.recv().await {
        match progress {
            FragmentProgress::PlanComplete {
                total,
                completed_indices,
                initial_concurrency,
            } => {
                // 覆盖真实分片数(替代 probe 估算)
                total_frags = total;
                if let Some(mut task) = task_repository.get_mut(&task_id) {
                    task.fragments_total = total;
                    task.active_concurrency = initial_concurrency;
                }
                // 初始化 FragmentStateStore
                let state = crate::projection::TaskFragmentState::from_plan(
                    total,
                    completed_indices.clone(),
                );
                fragment_state_store.init(&task_id, state);
                // 初始化 completed 集合(续传已完成分片)
                completed = completed_indices.into_iter().collect();
                // 触发广播(让前端拿到正确 total + concurrency)
                if let Some(ref callback) = on_progress {
                    callback(&task_id, None);
                }
                tracing::info!(
                    task_id = %task_id,
                    total_frags,
                    "PlanComplete 已处理"
                );
            }
            FragmentProgress::Started { fragment_index } => {
                // 标记分片开始下载(写入 FragmentStateStore.downloading_set)
                fragment_state_store.mark_downloading(&task_id, fragment_index);
                // 通知 broker 产生了 started delta,由 aggregator 推送给前端
                if let Some(ref callback) = on_progress {
                    callback(&task_id, Some(ProgressDelta::Started(fragment_index)));
                }
            }
            FragmentProgress::Chunk {
                fragment_index,
                completed: chunk_completed,
                fragment_downloaded,
            } => {
                event_count += 1;
                if chunk_completed {
                    completed.insert(fragment_index);
                    pending_completed.push(fragment_index);
                    // 更新 FragmentStateStore.done_set(内部同时清除 downloading_set)
                    fragment_state_store.mark_done(&task_id, fragment_index);
                }
                // 增量更新:先 insert 取出旧值计算差量,再按需清理 partial map。
                // 注意:完成事件必须在 insert 之后 remove。若先 remove 则 insert
                // 返回 None(old=0),会把整片大小再次累加,导致字节双重计数
                // (前端显示 ≈ 2× 文件大小,完成后被 file_size 覆盖跳回)。
                let old = frag_bytes
                    .insert(fragment_index, fragment_downloaded)
                    .unwrap_or(0);
                total_downloaded =
                    total_downloaded.saturating_add(fragment_downloaded.saturating_sub(old));
                if chunk_completed {
                    // 已完成的分片不再保留在 partial map 中
                    frag_bytes.remove(&fragment_index);
                }
                if event_count == 1 || event_count.is_multiple_of(50) {
                    tracing::info!(
                        event = event_count,
                        idx = fragment_index,
                        done = completed.len(),
                        total_frags,
                        total_downloaded,
                        "chunk reader 进度更新"
                    );
                }
                let frags_done = completed.len() as u32;

                // 计算速度:每 500ms 采样一次
                let now = tokio::time::Instant::now();
                let elapsed = now.duration_since(last_speed_time).as_secs_f64();
                let speed = if elapsed >= 0.5 {
                    let s = if elapsed > 0.0 {
                        ((total_downloaded as f64 - last_speed_sample as f64) / elapsed) as u64
                    } else {
                        0
                    };
                    last_speed_sample = total_downloaded;
                    last_speed_time = now;
                    s
                } else {
                    // 未到采样间隔,保持上次的 speed 值
                    task_repository.get(&task_id).map(|t| t.speed).unwrap_or(0)
                };

                {
                    if let Some(mut task) = task_repository.get_mut(&task_id) {
                        task.downloaded = total_downloaded;
                        task.fragments_done = frags_done;
                        task.fragments_total = total_frags;
                        task.speed = speed;
                        // 主进度使用字节比例而非分片比例
                        // clamp 到 [0.0, 1.0] 防止进度事件乱序导致进度条溢出
                        if let Some(file_size) = task.file_size.filter(|&s| s > 0) {
                            task.progress =
                                (total_downloaded as f64 / file_size as f64).clamp(0.0, 1.0);
                        } else if total_frags > 0 {
                            task.progress =
                                (frags_done as f64 / total_frags as f64).clamp(0.0, 1.0);
                        }
                    }
                }

                // Notify ProgressBroker of progress change
                if let Some(ref callback) = on_progress {
                    callback(
                        &task_id,
                        if chunk_completed {
                            Some(ProgressDelta::Completed(fragment_index))
                        } else {
                            None
                        },
                    );
                }

                // 批量 checkpoint(已完成分片)
                if chunk_completed
                    && (pending_completed.len() >= CHECKPOINT_BATCH_SIZE
                        || completed.len() as u32 == total_frags)
                {
                    let batch: Vec<u32> = std::mem::take(&mut pending_completed);
                    let downloaded = total_downloaded;
                    let partial = frag_bytes.clone();
                    let ts = task_store.clone();
                    let tid = task_id.clone();
                    match tokio::task::spawn_blocking(move || {
                        ts.update_snapshot(&tid, |snap| {
                            snap.completed_fragments.extend(batch);
                            snap.partial_fragments = partial;
                            snap.downloaded = downloaded;
                        })
                    })
                    .await
                    {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => {
                            tracing::warn!(task_id = %task_id, error = %e, "checkpoint 落盘失败");
                        }
                        Err(e) => {
                            tracing::warn!(task_id = %task_id, error = %e, "checkpoint spawn_blocking 失败");
                        }
                    }
                }

                // 字节级进度 checkpoint(未完整分片):按事件数周期落盘,
                // 避免崩溃后完整重下整个分片。
                partial_checkpoint_counter += 1;
                if partial_checkpoint_counter >= PARTIAL_CHECKPOINT_INTERVAL {
                    partial_checkpoint_counter = 0;
                    let downloaded = total_downloaded;
                    let partial = frag_bytes.clone();
                    let ts = task_store.clone();
                    let tid = task_id.clone();
                    match tokio::task::spawn_blocking(move || {
                        ts.update_snapshot(&tid, |snap| {
                            snap.partial_fragments = partial;
                            snap.downloaded = downloaded;
                        })
                    })
                    .await
                    {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => {
                            tracing::warn!(task_id = %task_id, error = %e, "partial checkpoint 落盘失败");
                        }
                        Err(e) => {
                            tracing::warn!(task_id = %task_id, error = %e, "partial checkpoint spawn_blocking 失败");
                        }
                    }
                }
            }
        }
    }
    // 确保 chunk reader 退出时剩余的 pending 分片也 flush
    if !pending_completed.is_empty() || !frag_bytes.is_empty() {
        let batch: Vec<u32> = pending_completed;
        let downloaded = total_downloaded;
        let partial = frag_bytes;
        let ts = task_store.clone();
        let tid = task_id.clone();
        match tokio::task::spawn_blocking(move || {
            ts.update_snapshot(&tid, |snap| {
                snap.completed_fragments.extend(batch);
                snap.partial_fragments = partial;
                snap.downloaded = downloaded;
            })
        })
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                tracing::warn!(task_id = %task_id, error = %e, "最终 checkpoint 落盘失败");
            }
            Err(e) => {
                tracing::warn!(task_id = %task_id, error = %e, "最终 checkpoint spawn_blocking 失败");
            }
        }
    }

    // 通知调用方 job 已完成
    let _ = done_tx.send(());
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::TaskInfo;
    use crate::repository::TaskRepository;
    use tachyon_core::types::DownloadState;

    /// 创建测试用 TaskStore
    fn test_task_store() -> Arc<TaskStore> {
        let tmp = tempfile::tempdir().unwrap();
        Arc::new(TaskStore::open(tmp.path()).unwrap())
    }

    #[tokio::test]
    async fn test_chunk_reader_pool_processes_progress() {
        let pool = ChunkReaderPool::new(2);
        pool.spawn_workers();
        let task_repository = TaskRepository::new();
        let task_store = test_task_store();
        let task_id = "test-pool-job".to_string();

        // 插入初始 TaskInfo
        task_repository.insert(
            task_id.clone(),
            TaskInfo {
                id: task_id.clone(),
                url: "https://example.com/file.bin".to_string(),
                file_name: "file.bin".to_string(),
                file_size: Some(1024),
                downloaded: 0,
                speed: 0,
                status: DownloadState::Downloading,
                progress: 0.0,
                fragments_total: 2,
                fragments_done: 0,
                active_concurrency: 0,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                save_path: "/tmp/file.bin".to_string(),
                error_reason: None,
                retry_count: 0,
                hf_meta: None,
                display_order: 0,
            },
        );

        // 创建进度通道
        let (progress_tx, progress_rx) = mpsc::channel::<FragmentProgress>(256);
        let (done_tx, done_rx) = oneshot::channel();

        let job = ChunkReaderJob {
            task_id: task_id.clone(),
            progress_rx,
            task_repository: task_repository.clone(),
            task_store,
            done_tx,
            on_progress: None,
            fragment_state_store: crate::projection::FragmentStateStore::new(),
        };

        // 提交 job
        pool.submit_async(job).await.unwrap();

        // 发送进度事件
        progress_tx
            .send(FragmentProgress::Chunk {
                fragment_index: 0,
                fragment_downloaded: 512,
                completed: true,
            })
            .await
            .unwrap();
        progress_tx
            .send(FragmentProgress::Chunk {
                fragment_index: 1,
                fragment_downloaded: 512,
                completed: true,
            })
            .await
            .unwrap();

        // 关闭发送端，触发 chunk reader 退出
        drop(progress_tx);

        // 等待 job 完成
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), done_rx).await;

        // 验证 TaskInfo 已更新
        let task = task_repository.get(&task_id).unwrap();
        assert_eq!(task.fragments_done, 2);
        assert_eq!(task.downloaded, 1024);
    }

    /// 验证分片完成事件不会导致字节双重计数。
    ///
    /// 回归场景:分片在流式下载过程中通过 `Chunk { completed: false }`
    /// 事件逐块累加 `total_downloaded`,分片结束时再发送
    /// `Chunk { completed: true, fragment_downloaded: 整片大小 }`。
    /// 若 app 层在完成事件时先 `remove` 再 `insert`,`insert` 返回 None(old=0),
    /// 会把整片大小再次累加,导致前端显示 ≈ 2× 文件大小。
    #[tokio::test]
    async fn test_chunk_completion_does_not_double_count_bytes() {
        let pool = ChunkReaderPool::new(1);
        pool.spawn_workers();
        let task_repository = TaskRepository::new();
        let task_store = test_task_store();
        let task_id = "test-double-count".to_string();
        let frag_size: u64 = 1_000;

        task_repository.insert(
            task_id.clone(),
            TaskInfo {
                id: task_id.clone(),
                url: "https://example.com/file.bin".to_string(),
                file_name: "file.bin".to_string(),
                file_size: Some(frag_size),
                downloaded: 0,
                speed: 0,
                status: DownloadState::Downloading,
                progress: 0.0,
                fragments_total: 1,
                fragments_done: 0,
                active_concurrency: 0,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                save_path: "/tmp/file.bin".to_string(),
                error_reason: None,
                retry_count: 0,
                hf_meta: None,
                display_order: 0,
            },
        );

        let (progress_tx, progress_rx) = mpsc::channel::<FragmentProgress>(256);
        let (done_tx, done_rx) = oneshot::channel();

        let job = ChunkReaderJob {
            task_id: task_id.clone(),
            progress_rx,
            task_repository: task_repository.clone(),
            task_store,
            done_tx,
            on_progress: None,
            fragment_state_store: crate::projection::FragmentStateStore::new(),
        };
        pool.submit_async(job).await.unwrap();

        // 流式增量:分片 0 在写入过程中逐块上报累计字节
        for partial in [200_u64, 500, 800] {
            progress_tx
                .send(FragmentProgress::Chunk {
                    fragment_index: 0,
                    completed: false,
                    fragment_downloaded: partial,
                })
                .await
                .unwrap();
        }
        // 分片完成事件:上报整片大小(与最后一个增量值一致)
        progress_tx
            .send(FragmentProgress::Chunk {
                fragment_index: 0,
                completed: true,
                fragment_downloaded: frag_size,
            })
            .await
            .unwrap();
        drop(progress_tx);

        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), done_rx).await;

        let task = task_repository.get(&task_id).unwrap();
        // 完成事件不应再次累加整片大小
        assert_eq!(
            task.downloaded, frag_size,
            "分片完成事件导致字节双重计数: got {} expected {}",
            task.downloaded, frag_size
        );
    }

    #[tokio::test]
    async fn test_chunk_reader_pool_multiple_jobs() {
        let pool = ChunkReaderPool::new(2);
        pool.spawn_workers();
        let task_repository = TaskRepository::new();
        let task_store = test_task_store();

        let mut done_rxs = Vec::new();

        for i in 0..3 {
            let task_id = format!("test-multi-{i}");
            task_repository.insert(
                task_id.clone(),
                TaskInfo {
                    id: task_id.clone(),
                    url: format!("https://example.com/file{i}.bin"),
                    file_name: format!("file{i}.bin"),
                    file_size: Some(256),
                    downloaded: 0,
                    speed: 0,
                    status: DownloadState::Downloading,
                    progress: 0.0,
                    fragments_total: 1,
                    fragments_done: 0,
                    active_concurrency: 0,
                    created_at: "2026-01-01T00:00:00Z".to_string(),
                    save_path: "/tmp/file.bin".to_string(),
                    error_reason: None,
                    retry_count: 0,
                    hf_meta: None,
                    display_order: 0,
                },
            );

            let (progress_tx, progress_rx) = mpsc::channel::<FragmentProgress>(256);
            let (done_tx, done_rx) = oneshot::channel();

            let job = ChunkReaderJob {
                task_id: task_id.clone(),
                progress_rx,
                task_repository: task_repository.clone(),
                task_store: task_store.clone(),
                done_tx,
                on_progress: None,
                fragment_state_store: crate::projection::FragmentStateStore::new(),
            };

            pool.submit_async(job).await.unwrap();
            done_rxs.push(done_rx);

            // 发送一个完成事件
            progress_tx
                .send(FragmentProgress::Chunk {
                    fragment_index: 0,
                    fragment_downloaded: 256,
                    completed: true,
                })
                .await
                .unwrap();
            drop(progress_tx);
        }

        // 等待所有 job 完成
        for done_rx in done_rxs {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), done_rx).await;
        }

        // 验证所有任务进度已更新
        for i in 0..3 {
            let task_id = format!("test-multi-{i}");
            let task = task_repository.get(&task_id).unwrap();
            assert_eq!(task.fragments_done, 1, "任务 {task_id} 应有 1 个分片完成");
            assert_eq!(task.downloaded, 256, "任务 {task_id} 应已下载 256 字节");
        }
    }

    /// 验证 Started 事件正确写入 FragmentStateStore.downloading_set,
    /// 且后续 Chunk{completed:true} 事件将分片从 downloading_set 移到 done_set。
    #[tokio::test]
    async fn test_started_event_populates_downloading_set() {
        let pool = ChunkReaderPool::new(1);
        pool.spawn_workers();
        let task_repository = TaskRepository::new();
        let task_store = test_task_store();
        let task_id = "test-started".to_string();

        task_repository.insert(
            task_id.clone(),
            TaskInfo {
                id: task_id.clone(),
                url: "https://example.com/file.bin".to_string(),
                file_name: "file.bin".to_string(),
                file_size: Some(1024),
                downloaded: 0,
                speed: 0,
                status: DownloadState::Downloading,
                progress: 0.0,
                fragments_total: 4,
                fragments_done: 0,
                active_concurrency: 0,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                save_path: "/tmp/file.bin".to_string(),
                error_reason: None,
                retry_count: 0,
                hf_meta: None,
                display_order: 0,
            },
        );

        let fragment_state_store = crate::projection::FragmentStateStore::new();
        // 先用 PlanComplete 初始化 fragment state
        fragment_state_store.init(
            &task_id,
            crate::projection::TaskFragmentState::from_plan(4, vec![]),
        );

        let (progress_tx, progress_rx) = mpsc::channel::<FragmentProgress>(256);
        let (done_tx, done_rx) = oneshot::channel();

        let job = ChunkReaderJob {
            task_id: task_id.clone(),
            progress_rx,
            task_repository: task_repository.clone(),
            task_store,
            done_tx,
            on_progress: None,
            fragment_state_store: fragment_state_store.clone(),
        };
        pool.submit_async(job).await.unwrap();

        // 发送 Started 事件:分片 0、1 开始下载
        progress_tx
            .send(FragmentProgress::Started { fragment_index: 0 })
            .await
            .unwrap();
        progress_tx
            .send(FragmentProgress::Started { fragment_index: 1 })
            .await
            .unwrap();
        // 分片 0 完成:应从 downloading_set 移到 done_set
        progress_tx
            .send(FragmentProgress::Chunk {
                fragment_index: 0,
                fragment_downloaded: 512,
                completed: true,
            })
            .await
            .unwrap();
        drop(progress_tx);

        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), done_rx).await;

        // 验证:分片 0 在 done_set,分片 1 仍在 downloading_set
        let state = fragment_state_store.get(&task_id).expect("应存在");
        assert!(state.done_set.contains(&0), "分片 0 完成后应在 done_set");
        assert!(
            !state.downloading_set.contains(&0),
            "分片 0 完成后不应在 downloading_set"
        );
        assert!(
            state.downloading_set.contains(&1),
            "分片 1 仍在下载,应在 downloading_set"
        );
    }
}
