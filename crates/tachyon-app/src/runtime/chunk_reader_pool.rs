//! 共享 ChunkReader 工作池
//!
//! 每个下载任务不再 spawn 独立的 chunk_reader_handle tokio task，
//! 而是通过共享的 ChunkReaderPool 提交进度处理任务。
//! 工作池固定 N 个 worker（N = max_concurrent_tasks），避免随任务数线性增长的 tokio task 数量。

use std::collections::BTreeSet;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use tachyon_core::FragmentProgress;

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
}

// ---------------------------------------------------------------------------
// ChunkReaderPool
// ---------------------------------------------------------------------------

/// 共享 ChunkReader 工作池
///
/// N 个 worker 从共享 mpsc receiver 拉取 job，执行进度处理逻辑。
/// 每个 job 独立运行，互不干扰。
pub struct ChunkReaderPool {
    /// 任务提交通道
    job_tx: mpsc::Sender<ChunkReaderJob>,
}

impl ChunkReaderPool {
    /// 创建新的 ChunkReaderPool
    ///
    /// `worker_count` 通常等于 max_concurrent_tasks，确保每个并发下载有一个 worker。
    pub fn new(worker_count: usize) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<ChunkReaderJob>(worker_count * 2);
        let job_rx = Arc::new(tokio::sync::Mutex::new(job_rx));

        for worker_id in 0..worker_count {
            let rx = job_rx.clone();
            tokio::spawn(async move {
                loop {
                    // 从共享 receiver 拉取 job
                    let job = {
                        let mut guard = rx.lock().await;
                        guard.recv().await
                    };
                    match job {
                        Some(job) => {
                            run_chunk_reader(job).await;
                        }
                        None => {
                            tracing::debug!(worker_id, "chunk reader worker 退出:通道已关闭");
                            break;
                        }
                    }
                }
            });
        }

        Self { job_tx }
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
    } = job;

    // 已完成分片集合,用于断点续传 checkpoint
    let mut completed: BTreeSet<u32> = BTreeSet::new();
    // 从 tasks 读取 probe 阶段已写入的 total_frags
    let total_frags = task_repository
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
        event_count += 1;
        if progress.completed {
            completed.insert(progress.fragment_index);
            pending_completed.push(progress.fragment_index);
            // 已完成的分片不再保留在 partial map 中
            frag_bytes.remove(&progress.fragment_index);
        }
        // 增量更新
        let old = frag_bytes
            .insert(progress.fragment_index, progress.fragment_downloaded)
            .unwrap_or(0);
        total_downloaded =
            total_downloaded.saturating_add(progress.fragment_downloaded.saturating_sub(old));
        if event_count == 1 || event_count.is_multiple_of(50) {
            tracing::info!(
                event = event_count,
                idx = progress.fragment_index,
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
                    task.progress = (total_downloaded as f64 / file_size as f64).clamp(0.0, 1.0);
                } else if total_frags > 0 {
                    task.progress = (frags_done as f64 / total_frags as f64).clamp(0.0, 1.0);
                }
            }
        }

        // 批量 checkpoint(已完成分片)
        if progress.completed
            && (pending_completed.len() >= CHECKPOINT_BATCH_SIZE
                || completed.len() as u32 == total_frags)
        {
            let batch: Vec<u32> = std::mem::take(&mut pending_completed);
            let downloaded = total_downloaded;
            let partial = frag_bytes.clone();
            if let Err(e) = task_store.update_snapshot(&task_id, |snap| {
                snap.completed_fragments.extend(batch);
                snap.partial_fragments = partial;
                snap.downloaded = downloaded;
            }) {
                tracing::warn!(task_id = %task_id, error = %e, "checkpoint 落盘失败");
            }
        }

        // 字节级进度 checkpoint(未完整分片):按事件数周期落盘,
        // 避免崩溃后完整重下整个分片。
        partial_checkpoint_counter += 1;
        if partial_checkpoint_counter >= PARTIAL_CHECKPOINT_INTERVAL {
            partial_checkpoint_counter = 0;
            let downloaded = total_downloaded;
            let partial = frag_bytes.clone();
            if let Err(e) = task_store.update_snapshot(&task_id, |snap| {
                snap.partial_fragments = partial;
                snap.downloaded = downloaded;
            }) {
                tracing::warn!(task_id = %task_id, error = %e, "partial checkpoint 落盘失败");
            }
        }
    }
    // 确保 chunk reader 退出时剩余的 pending 分片也 flush
    if !pending_completed.is_empty() || !frag_bytes.is_empty() {
        let batch: Vec<u32> = pending_completed;
        let downloaded = total_downloaded;
        let partial = frag_bytes;
        if let Err(e) = task_store.update_snapshot(&task_id, |snap| {
            snap.completed_fragments.extend(batch);
            snap.partial_fragments = partial;
            snap.downloaded = downloaded;
        }) {
            tracing::warn!(task_id = %task_id, error = %e, "最终 checkpoint 落盘失败");
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
                created_at: "2026-01-01T00:00:00Z".to_string(),
                save_path: "/tmp/file.bin".to_string(),
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
        };

        // 提交 job
        pool.submit_async(job).await.unwrap();

        // 发送进度事件
        progress_tx
            .send(FragmentProgress {
                fragment_index: 0,
                fragment_downloaded: 512,
                completed: true,
            })
            .await
            .unwrap();
        progress_tx
            .send(FragmentProgress {
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

    #[tokio::test]
    async fn test_chunk_reader_pool_multiple_jobs() {
        let pool = ChunkReaderPool::new(2);
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
                    created_at: "2026-01-01T00:00:00Z".to_string(),
                    save_path: "/tmp/file.bin".to_string(),
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
            };

            pool.submit_async(job).await.unwrap();
            done_rxs.push(done_rx);

            // 发送一个完成事件
            progress_tx
                .send(FragmentProgress {
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
}
