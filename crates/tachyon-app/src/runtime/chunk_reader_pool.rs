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

/// 活跃分片字节进度条目(传给 ProgressCallback 的切片元素)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FragmentByteEntry {
    pub index: u32,
    pub downloaded: u64,
}

/// 进度变化回调:参数为 (task_id, delta, fragment_bytes),
/// fragment_bytes 为已产生字节进度的活跃分片快照切片(刚 Started 尚无 Chunk 事件的分片不在其中)。
pub type ProgressCallback =
    Arc<dyn Fn(&str, Option<ProgressDelta>, &[FragmentByteEntry]) + Send + Sync>;

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
    /// 参数: (task_id, delta, fragment_bytes); delta None = 非状态变更事件(增量进度),
    /// fragment_bytes = 当前活跃分片字节快照切片
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

        // dispatcher: 从中心 channel 读取 job,分发到 per-worker channel。
        // 审计 M-04:禁止对固定 next_worker 阻塞 send——worker buffer=1 时若该
        // worker 正在处理长 job 且队列已满,dispatcher 会 HOL 饿死其他空闲 worker。
        // 策略:从 round-robin 起点 try_reserve;全满则 clone Sender 并发 reserve,
        // 第一个拿到 permit 的 worker 收下 job(job 本身不需 Clone)。
        tokio::spawn(async move {
            let mut next_worker = 0usize;
            let n = worker_txs.len();
            while let Some(job) = job_rx.recv().await {
                if n == 0 {
                    break;
                }
                let mut job_opt = Some(job);
                let mut delivered = false;
                for offset in 0..n {
                    let worker_id = (next_worker + offset) % n;
                    match worker_txs[worker_id].try_reserve() {
                        Ok(permit) => {
                            if let Some(j) = job_opt.take() {
                                permit.send(j);
                            }
                            next_worker = worker_id.wrapping_add(1);
                            delivered = true;
                            break;
                        }
                        Err(mpsc::error::TrySendError::Full(())) => {}
                        Err(mpsc::error::TrySendError::Closed(())) => {
                            tracing::debug!(
                                worker_id,
                                "chunk reader worker 已退出,try_reserve 忽略"
                            );
                        }
                    }
                }
                if delivered {
                    continue;
                }
                // 全部 worker 队列满:并发 reserve;第一个拿到 permit 且抢到 job 的 worker 收下。
                // job 放在 Mutex 中,避免 Permit 生命周期绑定本地 Sender 无法跨 future 返回。
                let job_cell = std::sync::Arc::new(tokio::sync::Mutex::new(job_opt.take()));
                let mut reserve_futs = Vec::with_capacity(n);
                for (worker_id, tx) in worker_txs.iter().enumerate() {
                    let tx = tx.clone();
                    let job_cell = std::sync::Arc::clone(&job_cell);
                    reserve_futs.push(Box::pin(async move {
                        let permit = tx.reserve().await.map_err(|_| ())?;
                        let mut guard = job_cell.lock().await;
                        if let Some(j) = guard.take() {
                            permit.send(j);
                            Ok(worker_id)
                        } else {
                            // 其他 worker 已交付;drop permit 释放预留槽
                            Err(())
                        }
                    }));
                }
                match futures::future::select_ok(reserve_futs).await {
                    Ok((worker_id, _)) => {
                        next_worker = worker_id.wrapping_add(1);
                    }
                    Err(_) => {
                        tracing::debug!("chunk reader 全部 worker 已退出,丢弃 job");
                    }
                }
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
                // 初始化 FragmentStateStore
                let state = crate::projection::TaskFragmentState::from_plan(
                    total,
                    completed_indices.clone(),
                );
                fragment_state_store.init(&task_id, state);
                // 初始化 completed 集合(续传已完成分片)
                completed = completed_indices.into_iter().collect();

                // 从快照种子 total_downloaded / frag_bytes,避免续传后进度回退与双重计数。
                // 有 snapshot 时用 snap.downloaded + partial_fragments;
                // 无 snapshot 时 fallback 到 task_repository 中现有 downloaded。
                match task_store.load_snapshot(&task_id) {
                    Ok(Some(snap)) => {
                        // 一致性校验:快照已完成分片必须 ⊆ PlanComplete 宣告的
                        // completed_indices。引擎在对象身份不兼容等场景会丢弃
                        // 续传数据全量重下,此时 completed_indices 不含快照分片;
                        // 照收快照种子会让 total_downloaded 虚高(种子+重下双计),
                        // 且 checkpoint 会把虚高值写回快照。校验失败种子归 0:
                        // repository 的 downloaded 正是被拒快照恢复出的同一个
                        // 陈旧值(重启经 snapshot_to_task_info 恢复、resume 不清
                        // 字节),引擎已明确从头重下,只能从零累计真实重下字节。
                        let snapshot_matches_plan = snap
                            .completed_fragments
                            .iter()
                            .all(|idx| completed.contains(idx));
                        if snapshot_matches_plan {
                            frag_bytes = snap.partial_fragments;
                            total_downloaded = snap.downloaded;
                        } else {
                            // 缺失索引(快照有而 plan 未采纳的分片),截断防爆日志
                            let missing: Vec<u32> = snap
                                .completed_fragments
                                .iter()
                                .filter(|idx| !completed.contains(idx))
                                .take(8)
                                .copied()
                                .collect();
                            tracing::warn!(
                                task_id = %task_id,
                                snap_completed = snap.completed_fragments.len(),
                                plan_completed = completed.len(),
                                missing_indices = ?missing,
                                "快照与 PlanComplete 续传决策不一致,种子归 0 全量重下"
                            );
                            total_downloaded = 0;
                        }
                    }
                    Ok(None) => {
                        total_downloaded = task_repository
                            .get(&task_id)
                            .map(|t| t.downloaded)
                            .unwrap_or(0);
                    }
                    Err(e) => {
                        tracing::warn!(
                            task_id = %task_id,
                            error = %e,
                            "PlanComplete 加载快照失败,fallback 到 repository downloaded"
                        );
                        total_downloaded = task_repository
                            .get(&task_id)
                            .map(|t| t.downloaded)
                            .unwrap_or(0);
                    }
                }

                let frags_done = completed.len() as u32;
                if let Some(mut task) = task_repository.get_mut(&task_id) {
                    task.downloaded = total_downloaded;
                    task.fragments_done = frags_done;
                    task.fragments_total = total;
                    task.active_concurrency = initial_concurrency;
                    // 与 Chunk 分支同一进度公式
                    if let Some(file_size) = task.file_size.filter(|&s| s > 0) {
                        task.progress =
                            (total_downloaded as f64 / file_size as f64).clamp(0.0, 1.0);
                    } else if total_frags > 0 {
                        task.progress = (frags_done as f64 / total_frags as f64).clamp(0.0, 1.0);
                    }
                }

                // 触发广播(delta=None + 当前 frag_bytes 快照)
                if let Some(ref callback) = on_progress {
                    let bytes_snapshot: Vec<FragmentByteEntry> = frag_bytes
                        .iter()
                        .map(|(&k, &v)| FragmentByteEntry {
                            index: k,
                            downloaded: v,
                        })
                        .collect();
                    callback(&task_id, None, &bytes_snapshot);
                }
                tracing::info!(
                    task_id = %task_id,
                    total_frags,
                    total_downloaded,
                    frags_done,
                    "PlanComplete 已处理"
                );
            }
            FragmentProgress::Started { fragment_index } => {
                // 标记分片开始下载(写入 FragmentStateStore.downloading_set)
                fragment_state_store.mark_downloading(&task_id, fragment_index);
                // 实时更新当前活跃并发数
                if let Some(mut task) = task_repository.get_mut(&task_id) {
                    task.active_concurrency =
                        fragment_state_store.active_downloading_count(&task_id);
                }
                // 通知 broker 产生了 started delta,由 aggregator 推送给前端
                if let Some(ref callback) = on_progress {
                    let bytes_snapshot: Vec<FragmentByteEntry> = frag_bytes
                        .iter()
                        .map(|(&k, &v)| FragmentByteEntry {
                            index: k,
                            downloaded: v,
                        })
                        .collect();
                    callback(
                        &task_id,
                        Some(ProgressDelta::Started(fragment_index)),
                        &bytes_snapshot,
                    );
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
                    // 实时更新当前活跃并发数
                    if let Some(mut task) = task_repository.get_mut(&task_id) {
                        task.active_concurrency =
                            fragment_state_store.active_downloading_count(&task_id);
                    }
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
                // 构造活跃分片字节快照(frag_bytes 当前状态),传给 ProgressBroker。
                // 完成事件已先 remove 该分片,故快照不含刚完成的分片。
                if let Some(ref callback) = on_progress {
                    let bytes_snapshot: Vec<FragmentByteEntry> = frag_bytes
                        .iter()
                        .map(|(&k, &v)| FragmentByteEntry {
                            index: k,
                            downloaded: v,
                        })
                        .collect();
                    callback(
                        &task_id,
                        if chunk_completed {
                            Some(ProgressDelta::Completed(fragment_index))
                        } else {
                            None
                        },
                        &bytes_snapshot,
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
    use std::collections::HashMap;

    use crate::commands::TaskInfo;
    use crate::repository::TaskRepository;
    use tachyon_core::types::DownloadState;
    use tachyon_store::TaskSnapshot;

    /// 创建测试用 TaskStore
    fn test_task_store() -> Arc<TaskStore> {
        let tmp = tempfile::tempdir().unwrap();
        Arc::new(TaskStore::open(tmp.path()).unwrap())
    }

    /// 创建带生命周期的 TaskStore(持有 TempDir 防止目录被清理)
    fn test_task_store_kept() -> (Arc<TaskStore>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(tmp.path()).unwrap());
        (store, tmp)
    }

    fn make_task_info(
        id: &str,
        file_size: u64,
        downloaded: u64,
        fragments_total: u32,
        fragments_done: u32,
    ) -> TaskInfo {
        TaskInfo {
            id: id.to_string(),
            url: format!("https://example.com/{id}.bin"),
            file_name: format!("{id}.bin"),
            file_size: Some(file_size),
            downloaded,
            speed: 0,
            status: DownloadState::Downloading,
            progress: if file_size > 0 {
                downloaded as f64 / file_size as f64
            } else {
                0.0
            },
            fragments_total,
            fragments_done,
            active_concurrency: 0,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            save_path: format!("/tmp/{id}.bin"),
            error_reason: None,
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
        }
    }

    fn make_resume_snapshot(
        task: &TaskInfo,
        completed_fragments: Vec<u32>,
        partial_fragments: HashMap<u32, u64>,
        fragment_size: u64,
    ) -> TaskSnapshot {
        TaskSnapshot {
            schema_version: tachyon_store::SNAPSHOT_SCHEMA_VERSION,
            revision: 0,
            id: task.id.clone(),
            url: task.url.clone(),
            save_path: task.save_path.clone(),
            file_name: task.file_name.clone(),
            file_size: task.file_size,
            downloaded: task.downloaded,
            completed_fragments,
            partial_fragments,
            total_fragments: task.fragments_total,
            fragment_size,
            status: task.status,
            etag: None,
            last_modified: None,
            content_length: task.file_size,
            supports_range: true,
            created_at: task.created_at.clone(),
            updated_at: task.created_at.clone(),
            fail_reason: None,
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
        }
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
                tags: vec![],
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
                tags: vec![],
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
                    tags: vec![],
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
                tags: vec![],
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

    /// 验证运行中 active_concurrency 基于真实 downloading_set 实时更新。
    #[tokio::test]
    async fn test_active_concurrency_tracks_downloading_set() {
        let pool = ChunkReaderPool::new(1);
        pool.spawn_workers();
        let task_repository = TaskRepository::new();
        let task_store = test_task_store();
        let task_id = "test-active-concurrency".to_string();

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
                tags: vec![],
                hf_meta: None,
                display_order: 0,
            },
        );

        let fragment_state_store = crate::projection::FragmentStateStore::new();
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

        const INITIAL_CONCURRENCY: u32 = 3;
        progress_tx
            .send(FragmentProgress::PlanComplete {
                total: 4,
                completed_indices: vec![],
                initial_concurrency: INITIAL_CONCURRENCY,
            })
            .await
            .unwrap();
        // PlanComplete 后 active_concurrency 应等于 initial_concurrency
        wait_for_active_concurrency(&task_repository, &task_id, INITIAL_CONCURRENCY).await;

        // 启动两个分片
        progress_tx
            .send(FragmentProgress::Started { fragment_index: 0 })
            .await
            .unwrap();
        progress_tx
            .send(FragmentProgress::Started { fragment_index: 1 })
            .await
            .unwrap();
        wait_for_active_concurrency(&task_repository, &task_id, 2).await;

        // 分片 0 完成后应剩 1 个
        progress_tx
            .send(FragmentProgress::Chunk {
                fragment_index: 0,
                fragment_downloaded: 512,
                completed: true,
            })
            .await
            .unwrap();
        wait_for_active_concurrency(&task_repository, &task_id, 1).await;

        // 分片 1 完成后应为 0
        progress_tx
            .send(FragmentProgress::Chunk {
                fragment_index: 1,
                fragment_downloaded: 512,
                completed: true,
            })
            .await
            .unwrap();
        wait_for_active_concurrency(&task_repository, &task_id, 0).await;

        drop(progress_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), done_rx).await;
    }

    /// 验证 ProgressCallback 第三参数接收到活跃分片字节快照
    #[tokio::test]
    async fn test_callback_receives_fragment_bytes() {
        use std::sync::Mutex;
        let pool = ChunkReaderPool::new(1);
        pool.spawn_workers();
        let task_repository = TaskRepository::new();
        let task_store = test_task_store();
        let task_id = "test-cb-bytes".to_string();

        task_repository.insert(
            task_id.clone(),
            TaskInfo {
                id: task_id.clone(),
                url: "https://example.com/file.bin".to_string(),
                file_name: "file.bin".to_string(),
                file_size: Some(1000),
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
                tags: vec![],
                hf_meta: None,
                display_order: 0,
            },
        );

        let captured: Arc<Mutex<Vec<FragmentByteEntry>>> = Arc::new(Mutex::new(vec![]));
        let captured_clone = captured.clone();
        let on_progress: ProgressCallback = Arc::new(move |_tid, _delta, bytes| {
            let mut g = captured_clone.lock().unwrap();
            g.clear();
            g.extend_from_slice(bytes);
        });

        let (progress_tx, progress_rx) = mpsc::channel::<FragmentProgress>(256);
        let (done_tx, done_rx) = oneshot::channel();
        let job = ChunkReaderJob {
            task_id: task_id.clone(),
            progress_rx,
            task_repository: task_repository.clone(),
            task_store,
            done_tx,
            on_progress: Some(on_progress),
            fragment_state_store: crate::projection::FragmentStateStore::new(),
        };
        pool.submit_async(job).await.unwrap();

        progress_tx
            .send(FragmentProgress::Chunk {
                fragment_index: 0,
                completed: false,
                fragment_downloaded: 300,
            })
            .await
            .unwrap();
        drop(progress_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), done_rx).await;

        let g = captured.lock().unwrap();
        assert!(
            g.iter().any(|e| e.index == 0 && e.downloaded == 300),
            "callback 应收到分片 0 字节 300,实际: {g:?}"
        );
    }

    async fn wait_for_active_concurrency(
        task_repository: &TaskRepository,
        task_id: &str,
        expected: u32,
    ) {
        for _ in 0..50 {
            if task_repository.get(task_id).map(|t| t.active_concurrency) == Some(expected) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let actual = task_repository
            .get(task_id)
            .map(|t| t.active_concurrency)
            .unwrap_or(u32::MAX);
        panic!("active_concurrency 未在预期时间内达到 {expected}, 实际 {actual}");
    }

    async fn wait_for_downloaded(task_repository: &TaskRepository, task_id: &str, expected: u64) {
        for _ in 0..50 {
            if task_repository.get(task_id).map(|t| t.downloaded) == Some(expected) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// PlanComplete 应从 TaskStore 快照种子 total_downloaded,
    /// 续传后新 Chunk 在种子值上累加,进度不回退。
    #[tokio::test]
    async fn plan_complete_seeds_downloaded_from_snapshot() {
        let pool = ChunkReaderPool::new(1);
        pool.spawn_workers();
        let task_repository = TaskRepository::new();
        let (task_store, _tmp) = test_task_store_kept();
        let task_id = "plan-complete-seed-dl".to_string();

        // 续传场景: 已完成 2 片(250+250) + partial 片 2 的 50 字节 = 750
        let task = make_task_info(&task_id, 1000, 750, 4, 2);
        task_repository.insert(task_id.clone(), task.clone());

        let mut partial = HashMap::new();
        partial.insert(2, 50_u64);
        let snapshot = make_resume_snapshot(&task, vec![0, 1], partial, 250);
        task_store.save_snapshot(&snapshot).unwrap();

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

        progress_tx
            .send(FragmentProgress::PlanComplete {
                total: 4,
                completed_indices: vec![0, 1],
                initial_concurrency: 2,
            })
            .await
            .unwrap();

        // 续传后新分片 3 上报 10 字节: 在种子 750 基础上 +10 = 760
        progress_tx
            .send(FragmentProgress::Chunk {
                fragment_index: 3,
                fragment_downloaded: 10,
                completed: false,
            })
            .await
            .unwrap();

        wait_for_downloaded(&task_repository, &task_id, 760).await;
        drop(progress_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), done_rx).await;

        let task = task_repository.get(&task_id).unwrap();
        assert_eq!(
            task.downloaded, 760,
            "PlanComplete 应从 snapshot 种子 downloaded=750, 再 +Chunk(10) => 760; 实际 {}",
            task.downloaded
        );
        assert_eq!(
            task.fragments_done, 2,
            "completed_indices 应反映 fragments_done=2"
        );
        assert!(
            (task.progress - 0.76).abs() < 0.001,
            "progress 应约 0.76, 实际 {}",
            task.progress
        );
    }

    /// PlanComplete 应种子 partial_fragments,后续 Chunk 按差量累加,不双重计数。
    #[tokio::test]
    async fn plan_complete_seeds_partial_bytes_no_double_count() {
        let pool = ChunkReaderPool::new(1);
        pool.spawn_workers();
        let task_repository = TaskRepository::new();
        let (task_store, _tmp) = test_task_store_kept();
        let task_id = "plan-complete-seed-partial".to_string();

        // 续传: 完成片 0(250) + partial 片 1 的 100 = 350 / 500
        let task = make_task_info(&task_id, 500, 350, 2, 1);
        task_repository.insert(task_id.clone(), task.clone());

        let mut partial = HashMap::new();
        partial.insert(1, 100_u64);
        let snapshot = make_resume_snapshot(&task, vec![0], partial, 250);
        task_store.save_snapshot(&snapshot).unwrap();

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

        progress_tx
            .send(FragmentProgress::PlanComplete {
                total: 2,
                completed_indices: vec![0],
                initial_concurrency: 1,
            })
            .await
            .unwrap();

        // 片 1 从已种子的 100 推进到 150: delta=50, 期望 350+50=400
        progress_tx
            .send(FragmentProgress::Chunk {
                fragment_index: 1,
                fragment_downloaded: 150,
                completed: false,
            })
            .await
            .unwrap();

        wait_for_downloaded(&task_repository, &task_id, 400).await;
        drop(progress_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), done_rx).await;

        let task = task_repository.get(&task_id).unwrap();
        assert_eq!(
            task.downloaded, 400,
            "partial 已种子 100 时 Chunk(150) 只应 +50 => 400; 实际 {}",
            task.downloaded
        );
    }

    /// 快照与引擎续传决策失配时不得采用快照种子,也不得取 repository 值。
    ///
    /// 场景:对象身份不兼容,引擎丢弃续传数据全量重下,PlanComplete 的
    /// completed_indices 为空。真实流程中 repository 的 downloaded 正是
    /// 被拒快照恢复出的同一个陈旧值(重启经 snapshot_to_task_info 恢复、
    /// resume 不清字节,此处 750);快照亦然(completed=[0,1], downloaded=750)。
    /// 无论取哪个,total_downloaded 都虚高(750 + 重下字节),后续 checkpoint
    /// 还会把虚高值写回快照。期望:种子直接归 0,从头累计真实重下字节。
    #[tokio::test]
    async fn plan_complete_rejects_snapshot_seed_when_plan_discards_resume() {
        let pool = ChunkReaderPool::new(1);
        pool.spawn_workers();
        let task_repository = TaskRepository::new();
        let (task_store, _tmp) = test_task_store_kept();
        let task_id = "plan-complete-seed-mismatch".to_string();

        // 真实流程:repository 与快照同为陈旧值 750(重启恢复 + resume 不清字节)
        let task = make_task_info(&task_id, 1000, 750, 4, 2);
        task_repository.insert(task_id.clone(), task.clone());

        let mut partial = HashMap::new();
        partial.insert(2, 50_u64);
        let snapshot = make_resume_snapshot(&task, vec![0, 1], partial, 250);
        task_store.save_snapshot(&snapshot).unwrap();

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

        // 引擎未采纳任何续传分片:completed_indices 为空
        progress_tx
            .send(FragmentProgress::PlanComplete {
                total: 4,
                completed_indices: vec![],
                initial_concurrency: 2,
            })
            .await
            .unwrap();

        // 重下的分片 0 上报 100 字节:应从 0 起 +100 = 100,而非种子 750+100=850
        progress_tx
            .send(FragmentProgress::Chunk {
                fragment_index: 0,
                fragment_downloaded: 100,
                completed: false,
            })
            .await
            .unwrap();

        wait_for_downloaded(&task_repository, &task_id, 100).await;
        drop(progress_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), done_rx).await;

        let task = task_repository.get(&task_id).unwrap();
        assert_eq!(
            task.downloaded, 100,
            "快照 completed=[0,1] 不在 PlanComplete completed_indices=[] 内,应放弃快照种子; \
             期望 0+100=100, 实际 {}",
            task.downloaded
        );
        assert_eq!(
            task.fragments_done, 0,
            "completed_indices 为空时 fragments_done 应为 0"
        );
    }

    /// 无快照时 fallback 到 repository 现有 downloaded,而非从 0 起算。
    ///
    /// 场景:快照文件丢失/未写入,但任务列表已有累计字节(如内存态恢复)。
    /// 期望:PlanComplete 后 downloaded 保持 repository 值,Chunk 在其上累加。
    #[tokio::test]
    async fn plan_complete_falls_back_to_repository_downloaded_without_snapshot() {
        let pool = ChunkReaderPool::new(1);
        pool.spawn_workers();
        let task_repository = TaskRepository::new();
        let (task_store, _tmp) = test_task_store_kept();
        let task_id = "plan-complete-no-snapshot".to_string();

        // 不写任何快照;repository 已有 300 字节
        let task = make_task_info(&task_id, 1000, 300, 4, 1);
        task_repository.insert(task_id.clone(), task);

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

        progress_tx
            .send(FragmentProgress::PlanComplete {
                total: 4,
                completed_indices: vec![0],
                initial_concurrency: 2,
            })
            .await
            .unwrap();

        // 分片 1 上报 50 字节: 应在 fallback 300 上 +50 = 350
        progress_tx
            .send(FragmentProgress::Chunk {
                fragment_index: 1,
                fragment_downloaded: 50,
                completed: false,
            })
            .await
            .unwrap();

        wait_for_downloaded(&task_repository, &task_id, 350).await;
        drop(progress_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), done_rx).await;

        let task = task_repository.get(&task_id).unwrap();
        assert_eq!(
            task.downloaded, 350,
            "无快照应 fallback 到 repository downloaded=300, 再 +50 => 350; 实际 {}",
            task.downloaded
        );
    }

    /// 快照文件损坏(load_snapshot 返回 Err)时 fallback 到 repository downloaded。
    ///
    /// 场景:快照 JSON 损坏,无法解析;此时引擎续传决策未知,取 repository
    /// 现值(与无快照分支同语义),不因损坏而 panic 或归零已有进度。
    #[tokio::test]
    async fn plan_complete_falls_back_to_repository_on_snapshot_load_error() {
        let pool = ChunkReaderPool::new(1);
        pool.spawn_workers();
        let task_repository = TaskRepository::new();
        let (task_store, tmp) = test_task_store_kept();
        let task_id = "plan-complete-corrupt-snapshot".to_string();

        // repository 已有 300 字节
        let task = make_task_info(&task_id, 1000, 300, 4, 1);
        task_repository.insert(task_id.clone(), task.clone());

        // 先写合法快照再损坏文件,使 load_snapshot 走 Err 分支
        let snapshot = make_resume_snapshot(&task, vec![0], HashMap::new(), 250);
        task_store.save_snapshot(&snapshot).unwrap();
        std::fs::write(
            tmp.path().join(format!("task_{task_id}.json")),
            "{ 这不是合法 JSON",
        )
        .unwrap();

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

        progress_tx
            .send(FragmentProgress::PlanComplete {
                total: 4,
                completed_indices: vec![0],
                initial_concurrency: 2,
            })
            .await
            .unwrap();

        // 分片 1 上报 50 字节: Err fallback 取 repository 300, +50 = 350
        progress_tx
            .send(FragmentProgress::Chunk {
                fragment_index: 1,
                fragment_downloaded: 50,
                completed: false,
            })
            .await
            .unwrap();

        wait_for_downloaded(&task_repository, &task_id, 350).await;
        drop(progress_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), done_rx).await;

        let task = task_repository.get(&task_id).unwrap();
        assert_eq!(
            task.downloaded, 350,
            "快照损坏应 fallback 到 repository downloaded=300, 再 +50 => 350; 实际 {}",
            task.downloaded
        );
    }

    /// 快照 partial 为空但 completed 非空时,按快照 downloaded 正常种子。
    ///
    /// 场景:上次退出时所有 partial 已刷成完整分片。期望:种子 500,
    /// 后续 Chunk 在其上累加,frag_bytes 从空集开始。
    #[tokio::test]
    async fn plan_complete_seeds_with_empty_partial_and_nonempty_completed() {
        let pool = ChunkReaderPool::new(1);
        pool.spawn_workers();
        let task_repository = TaskRepository::new();
        let (task_store, _tmp) = test_task_store_kept();
        let task_id = "plan-complete-empty-partial".to_string();

        // 已完成 2 片共 500,无 partial
        let task = make_task_info(&task_id, 1000, 500, 4, 2);
        task_repository.insert(task_id.clone(), task.clone());

        let snapshot = make_resume_snapshot(&task, vec![0, 1], HashMap::new(), 250);
        task_store.save_snapshot(&snapshot).unwrap();

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

        progress_tx
            .send(FragmentProgress::PlanComplete {
                total: 4,
                completed_indices: vec![0, 1],
                initial_concurrency: 2,
            })
            .await
            .unwrap();

        // 新分片 2 上报 100 字节: 种子 500 + 100 = 600
        progress_tx
            .send(FragmentProgress::Chunk {
                fragment_index: 2,
                fragment_downloaded: 100,
                completed: false,
            })
            .await
            .unwrap();

        wait_for_downloaded(&task_repository, &task_id, 600).await;
        drop(progress_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), done_rx).await;

        let task = task_repository.get(&task_id).unwrap();
        assert_eq!(
            task.downloaded, 600,
            "partial 为空 + completed=[0,1] 应种子 500, 再 +100 => 600; 实际 {}",
            task.downloaded
        );
        assert_eq!(task.fragments_done, 2);
    }

    /// snap.downloaded==0 且 completed 非空时,按快照原值种子 0,不编造字节数。
    ///
    /// 场景:快照 completed_fragments 已写入但 downloaded 字段未刷(异常退出
    /// 窗口期)。期望:total_downloaded 取快照原值 0,不从
    /// completed.len()×fragment_size 推算虚构字节。
    #[tokio::test]
    async fn plan_complete_seeds_zero_downloaded_without_fabricating_size() {
        let pool = ChunkReaderPool::new(1);
        pool.spawn_workers();
        let task_repository = TaskRepository::new();
        let (task_store, _tmp) = test_task_store_kept();
        let task_id = "plan-complete-zero-downloaded".to_string();

        // 快照 completed=[0,1] 但 downloaded=0
        let task = make_task_info(&task_id, 1000, 0, 4, 2);
        task_repository.insert(task_id.clone(), task.clone());

        let snapshot = make_resume_snapshot(&task, vec![0, 1], HashMap::new(), 250);
        task_store.save_snapshot(&snapshot).unwrap();

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

        progress_tx
            .send(FragmentProgress::PlanComplete {
                total: 4,
                completed_indices: vec![0, 1],
                initial_concurrency: 2,
            })
            .await
            .unwrap();

        drop(progress_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), done_rx).await;

        let task = task_repository.get(&task_id).unwrap();
        assert_eq!(
            task.downloaded, 0,
            "snap.downloaded==0 时应按原值种子 0, 不应编造 2×250=500; 实际 {}",
            task.downloaded
        );
        assert_eq!(
            task.fragments_done, 2,
            "completed_indices=[0,1] 仍应反映 fragments_done=2"
        );
    }
}
