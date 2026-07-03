# 分片真实状态前端对接 实现计划

> **For agentic workers:** REQUIRED SUB-SILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让前端拿到 `plan_fragments` 的真实分片数和逐分片完成状态，替代 probe 估算值和顺序推算。

**Architecture:** FragmentProgress 枚举化（PlanComplete + Chunk 两变体），plan() 末尾通过已有 progress_tx 发 PlanComplete 事件携带真实 total + 续传 done 集 + 初始并发度；chunk reader match 两分支处理；ProgressBroker 增量收集 completedDelta 随 progress-update 发出；前端 taskFragments store 维护真实 doneSet，ChunkMatrix DOM/Canvas 双模式读真实数据。

**Tech Stack:** Rust + Tauri v2（后端），SolidJS + TypeScript（前端）

## Global Constraints

- cargo clippy MUST 零警告（`-D warnings`）
- 测试覆盖率 MUST >= 90%（协议层/网络 IO/Tauri 命令排除）
- 所有 unsafe 代码 MUST 有 Safety 注释
- 注释/文档/提交信息使用中文，代码标识符使用英文，不使用 emoji
- 前端 MUST 使用 Bun + Tauri v2
- 提交格式：`<类型>(<范围>): <简要描述>`（中文）

---

## File Structure

### 后端新建文件
- `crates/tachyon-app/src/projection/fragment_state_store.rs` — TaskFragmentState + FragmentStateStore
- `crates/tachyon-app/src/commands/fragment_commands.rs` — get_task_fragments command

### 后端修改文件
- `crates/tachyon-core/src/types.rs` — FragmentProgress struct→enum
- `crates/tachyon-engine/src/downloader.rs` — 3 处 send 点改构造 + plan() 末尾发 PlanComplete
- `crates/tachyon-app/src/runtime/chunk_reader_pool.rs` — ChunkReaderJob 加字段 + match 两分支
- `crates/tachyon-app/src/runtime/download_session.rs` — on_progress 签名 + job 构造
- `crates/tachyon-app/src/projection/progress_broker.rs` — pending_deltas + build_progress_event 签名
- `crates/tachyon-app/src/projection/mod.rs` — 导出 fragment_state_store
- `crates/tachyon-app/src/commands/progress_commands.rs` — build_initial_progress_event 加字段
- `crates/tachyon-app/src/commands/mod.rs` — TaskInfo/DownloadProgress/TaskProgress 加字段 + AppState 加 store + cleanup 改
- `crates/tachyon-app/src/commands/states.rs` — 如有 AppState 构造相关
- `crates/tachyon-app/src/lib.rs` — 注册 get_task_fragments

### 前端新建文件
- `frontend/src/stores/taskFragments.ts` — 分片 store

### 前端修改文件
- `frontend/src/types.ts` — ProgressPayload 加字段 + TaskFragmentsView 新增
- `frontend/src/stores/downloads.ts` — updateProgress 合并 delta + fragmentsTotal
- `frontend/src/components/ChunkMatrix.tsx` — DOM + Canvas 双模式改读 doneSet
- `frontend/src/components/DetailPanel.tsx` — 生命周期加载/清理 + props 加 taskId
- `frontend/src/api/invoke.ts` — getTaskFragments

---

## Task 1: FragmentProgress 枚举化（tachyon-core）

**Files:**
- Modify: `crates/tachyon-core/src/types.rs:355-367`
- Test: `crates/tachyon-core/src/types.rs`（内联测试模块）

**Interfaces:**
- Produces: `pub enum FragmentProgress { PlanComplete{total,completed_indices,initial_concurrency}, Chunk{fragment_index,completed,fragment_downloaded} }`

- [ ] **Step 1: 写失败测试**

在 `crates/tachyon-core/src/types.rs` 的 `#[cfg(test)] mod tests` 内追加：

```rust
#[test]
fn test_fragment_progress_plan_complete_serialization() {
    let progress = FragmentProgress::PlanComplete {
        total: 16,
        completed_indices: vec![0, 1, 2],
        initial_concurrency: 4,
    };
    let json = serde_json::to_string(&progress).unwrap();
    assert!(json.contains("\"total\":16"));
    assert!(json.contains("\"completedIndices\":[0,1,2]"));
    assert!(json.contains("\"initialConcurrency\":4"));
    let de: FragmentProgress = serde_json::from_str(&json).unwrap();
    match de {
        FragmentProgress::PlanComplete { total, completed_indices, initial_concurrency } => {
            assert_eq!(total, 16);
            assert_eq!(completed_indices, vec![0, 1, 2]);
            assert_eq!(initial_concurrency, 4);
        }
        FragmentProgress::Chunk { .. } => panic!("应为 PlanComplete"),
    }
}

#[test]
fn test_fragment_progress_chunk_serialization() {
    let progress = FragmentProgress::Chunk {
        fragment_index: 5,
        completed: true,
        fragment_downloaded: 1024,
    };
    let json = serde_json::to_string(&progress).unwrap();
    assert!(json.contains("\"fragmentIndex\":5"));
    assert!(json.contains("\"completed\":true"));
    assert!(json.contains("\"fragmentDownloaded\":1024"));
    let de: FragmentProgress = serde_json::from_str(&json).unwrap();
    match de {
        FragmentProgress::Chunk { fragment_index, completed, fragment_downloaded } => {
            assert_eq!(fragment_index, 5);
            assert!(completed);
            assert_eq!(fragment_downloaded, 1024);
        }
        FragmentProgress::PlanComplete { .. } => panic!("应为 Chunk"),
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo nextest run -p tachyon-core -- test_fragment_progress --exact`
Expected: 编译失败（FragmentProgress 仍是 struct，无 PlanComplete/Chunk 变体）

- [ ] **Step 3: 替换 FragmentProgress 定义**

在 `crates/tachyon-core/src/types.rs` 中，找到原 `FragmentProgress` struct（约 355-367 行，以 `pub struct FragmentProgress` 开头，含 `fragment_index`/`completed`/`fragment_downloaded` 三字段），替换为：

```rust
/// 分片进度事件
///
/// 通过 `progress_tx` 通道发送给上层(tachyon-app)。
/// 两变体:控制帧(PlanComplete,一次性可靠)、数据帧(Chunk,高频可丢)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FragmentProgress {
    /// plan 完成:携带真实分片总数 + 续传已完成索引 + 初始并发度
    ///
    /// 仅在 `DownloadTask::plan()` 末尾发送一次。
    /// 用 `send().await`——此时 channel 必为空(plan 是第一个事件),不会阻塞。
    PlanComplete {
        /// 真实分片总数(来自 plan_fragments,非 probe 估算)
        total: u32,
        /// 续传恢复的已完成分片索引(state==Done 的 index 列表)
        completed_indices: Vec<u32>,
        /// 初始并发度(调度器 recommendation.concurrency)
        initial_concurrency: u32,
    },
    /// 分片下载进度(原 struct 三字段,语义不变)
    ///
    /// 增量用 `try_send`(可丢),完成用 `send().await`
    Chunk {
        fragment_index: u32,
        completed: bool,
        fragment_downloaded: u64,
    },
}
```

同时删除原 struct 上方的旧文档注释（以 `/// 通过 progress_tx 通道发送` 开头的那段，约 355-358 行）。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo nextest run -p tachyon-core -- test_fragment_progress --exact`
Expected: 两个测试 PASS

- [ ] **Step 5: 确认 core 编译（engine 会因 Copy break 报错，正常）**

Run: `cargo build -p tachyon-core`
Expected: core 编译通过（engine 此时会报错，因为下游还在用 struct 语法，Task 2 修复）

- [ ] **Step 6: 提交**

```bash
git add crates/tachyon-core/src/types.rs
git commit -m "refactor(core): FragmentProgress 从 struct 改为 enum(PlanComplete+Chunk)"
```

---

## Task 2: engine 侧 3 处 send 点改构造 + plan() 末尾发 PlanComplete

**Files:**
- Modify: `crates/tachyon-engine/src/downloader.rs:1634-1653`（report_progress）
- Modify: `crates/tachyon-engine/src/downloader.rs:1859-1870`（分片完成 send）
- Modify: `crates/tachyon-engine/src/downloader.rs:852-854`（plan 末尾，新增）
- Test: `crates/tachyon-engine/src/downloader.rs` 内联测试

**Interfaces:**
- Consumes: `FragmentProgress::Chunk{..}` / `FragmentProgress::PlanComplete{..}`（来自 Task 1）
- Produces: plan() 末尾通过 progress_tx 发 PlanComplete 事件

- [ ] **Step 1: 写失败测试**

在 `crates/tachyon-engine/src/downloader.rs` 的 `#[cfg(test)]` 模块内追加（如果已有测试模块就追加到其中）：

```rust
#[tokio::test]
async fn test_plan_sends_plan_complete_event() {
    use tachyon_core::FragmentProgress;
    use tachyon_protocol::Protocol;

    // 构造最小可用的 DownloadTask(需 mock protocol 返回 supports_range=true)
    // 用已有的测试辅助函数构造 task
    let mut task = create_test_task_with_range_support("http://example.com/file.bin", 1024 * 1024 * 10).await;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<FragmentProgress>(256);
    task.set_progress_sender(tx);

    task.probe().await.unwrap();
    task.plan().await.unwrap();

    // 第一个事件应为 PlanComplete
    let event = rx.recv().await.expect("应收到 PlanComplete 事件");
    match event {
        FragmentProgress::PlanComplete { total, completed_indices, initial_concurrency } => {
            assert!(total > 0, "total 应为真实分片数");
            assert!(completed_indices.is_empty(), "非续传场景 completed_indices 应为空");
            assert!(initial_concurrency > 0, "initial_concurrency 应为调度器建议值");
        }
        FragmentProgress::Chunk { .. } => panic!("第一个事件应为 PlanComplete,不是 Chunk"),
    }
}
```

注：`create_test_task_with_range_support` 若不存在，参考现有测试中的 task 构造辅助函数实现。如果该辅助函数创建成本过高，改为在 `plan()` 末尾直接断言 `self.progress_tx` 的 channel 收到消息——用更简单的集成方式。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo nextest run -p tachyon-engine -- test_plan_sends_plan_complete --exact`
Expected: 编译失败（report_progress/分片完成 send 仍用 struct 语法）

- [ ] **Step 3: 修改 report_progress 构造体**

在 `crates/tachyon-engine/src/downloader.rs` 约 1640 行，将：

```rust
            match tx.try_send(FragmentProgress {
                fragment_index: frag_index,
                completed: false,
                fragment_downloaded: total_written,
            }) {
```

替换为：

```rust
            match tx.try_send(FragmentProgress::Chunk {
                fragment_index: frag_index,
                completed: false,
                fragment_downloaded: total_written,
            }) {
```

- [ ] **Step 4: 修改分片完成 send 构造体**

在 `crates/tachyon-engine/src/downloader.rs` 约 1862 行，将：

```rust
                .send(FragmentProgress {
                    fragment_index: frag_index,
                    completed: true,
                    fragment_downloaded: total_written,
                })
```

替换为：

```rust
                .send(FragmentProgress::Chunk {
                    fragment_index: frag_index,
                    completed: true,
                    fragment_downloaded: total_written,
                })
```

- [ ] **Step 5: 在 plan() 末尾新增 PlanComplete send**

在 `crates/tachyon-engine/src/downloader.rs` 的 `plan()` 方法内，约 853 行 `Ok(fragments)` 之前插入：

```rust
        // 发送 PlanComplete 事件:携带真实分片总数 + 续传已完成索引 + 初始并发度
        // 此时 channel 必为空(plan 是第一个事件),send().await 不会阻塞持锁的 run()
        if let Some(tx) = &self.progress_tx {
            let total = self.fragments.len() as u32;
            let completed_indices: Vec<u32> = self
                .fragments
                .iter()
                .filter(|f| f.state == crate::fragment::FragmentState::Done)
                .map(|f| f.info.index)
                .collect();
            if let Err(e) = tx
                .send(FragmentProgress::PlanComplete {
                    total,
                    completed_indices,
                    initial_concurrency: recommendation.concurrency,
                })
                .await
            {
                warn!(error = %e, "PlanComplete 事件发送失败");
            }
        }

```

- [ ] **Step 6: 运行测试确认通过**

Run: `cargo nextest run -p tachyon-engine -- test_plan_sends_plan_complete --exact`
Expected: PASS

- [ ] **Step 7: 确认 engine 全量编译**

Run: `cargo build -p tachyon-engine`
Expected: 编译通过（app 层可能因 chunk_reader_pool 的 match 报错，Task 3 修复）

- [ ] **Step 8: clippy 检查**

Run: `cargo clippy -p tachyon-core -p tachyon-engine --all-targets -- -D warnings`
Expected: 零警告

- [ ] **Step 9: 提交**

```bash
git add crates/tachyon-engine/src/downloader.rs
git commit -m "feat(engine): plan 末尾发 PlanComplete 事件 + 3 处 send 点改 Chunk 变体"
```

---

## Task 3: FragmentStateStore + ChunkReaderJob 扩展

**Files:**
- Create: `crates/tachyon-app/src/projection/fragment_state_store.rs`
- Modify: `crates/tachyon-app/src/projection/mod.rs`
- Modify: `crates/tachyon-app/src/runtime/chunk_reader_pool.rs:28-38`（ChunkReaderJob）
- Modify: `crates/tachyon-app/src/runtime/chunk_reader_pool.rs:161-313`（run_chunk_reader）
- Test: `crates/tachyon-app/src/projection/fragment_state_store.rs`（内联测试）

**Interfaces:**
- Consumes: `FragmentProgress`（Task 1）、`TaskRepository`
- Produces: `FragmentStateStore`（长存于 AppState）、`ChunkReaderJob.fragment_state_store` 字段

- [ ] **Step 1: 创建 fragment_state_store.rs 并写测试**

创建 `crates/tachyon-app/src/projection/fragment_state_store.rs`：

```rust
//! 分片状态存储
//!
//! 维护每个任务的真实分片总数和已完成分片索引集合,
//! 供 get_task_fragments command 查询和 ChunkMatrix 渲染使用。
//! 由 PlanComplete 事件初始化,Chunk::completed 事件增量更新,
//! 任务终态时由 cleanup_runtime 移除。

use std::collections::BTreeSet;

use dashmap::DashMap;
use std::sync::Arc;

/// 单个任务的分片运行时状态(内存,随任务生命周期)
pub struct TaskFragmentState {
    /// 真实分片总数(来自 plan_fragments,非 probe 估算)
    pub total: u32,
    /// 已完成分片索引集合
    pub done_set: BTreeSet<u32>,
}

impl TaskFragmentState {
    /// 从 PlanComplete 事件构造
    pub fn from_plan(total: u32, completed_indices: Vec<u32>) -> Self {
        Self {
            total,
            done_set: completed_indices.into_iter().collect(),
        }
    }

    /// 标记分片完成
    pub fn mark_done(&mut self, index: u32) {
        self.done_set.insert(index);
    }
}

/// 全局分片状态存储,长存于 AppState
///
/// key = task_id, value = TaskFragmentState。
/// 任务进入 downloading 时由 PlanComplete 初始化,
/// 任务终态时由 cleanup_runtime 移除。
#[derive(Clone, Default)]
pub struct FragmentStateStore(Arc<DashMap<String, TaskFragmentState>>);

impl FragmentStateStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 初始化任务分片状态(PlanComplete 事件触发)
    /// 若已存在则覆盖(防止重试场景残留旧状态)
    pub fn init(&self, task_id: &str, state: TaskFragmentState) {
        self.0.insert(task_id.to_string(), state);
    }

    /// 标记分片完成(Chunk::completed 事件触发)
    pub fn mark_done(&self, task_id: &str, index: u32) {
        if let Some(mut state) = self.0.get_mut(task_id) {
            state.mark_done(index);
        }
    }

    /// 查询任务分片状态(get_task_fragments command 调用)
    pub fn get(&self, task_id: &str) -> Option<dashmap::mapref::one::Ref<'_, String, TaskFragmentState>> {
        self.0.get(task_id)
    }

    /// 移除任务分片状态(cleanup_runtime 调用)
    pub fn remove(&self, task_id: &str) {
        self.0.remove(task_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_plan_empty() {
        let state = TaskFragmentState::from_plan(16, vec![]);
        assert_eq!(state.total, 16);
        assert!(state.done_set.is_empty());
    }

    #[test]
    fn test_from_plan_with_completed() {
        let state = TaskFragmentState::from_plan(16, vec![0, 1, 2]);
        assert_eq!(state.total, 16);
        assert_eq!(state.done_set.len(), 3);
        assert!(state.done_set.contains(&1));
    }

    #[test]
    fn test_mark_done() {
        let mut state = TaskFragmentState::from_plan(16, vec![]);
        state.mark_done(5);
        assert!(state.done_set.contains(&5));
        // 幂等:重复 mark_done 不增加
        state.mark_done(5);
        assert_eq!(state.done_set.len(), 1);
    }

    #[test]
    fn test_store_init_and_get() {
        let store = FragmentStateStore::new();
        store.init("task1", TaskFragmentState::from_plan(8, vec![0]));
        let state = store.get("task1").expect("应存在");
        assert_eq!(state.total, 8);
        assert_eq!(state.done_set.len(), 1);
    }

    #[test]
    fn test_store_mark_done() {
        let store = FragmentStateStore::new();
        store.init("task1", TaskFragmentState::from_plan(8, vec![]));
        store.mark_done("task1", 3);
        let state = store.get("task1").expect("应存在");
        assert!(state.done_set.contains(&3));
    }

    #[test]
    fn test_store_remove() {
        let store = FragmentStateStore::new();
        store.init("task1", TaskFragmentState::from_plan(8, vec![]));
        store.remove("task1");
        assert!(store.get("task1").is_none());
    }

    #[test]
    fn test_store_overwrite_on_reinit() {
        let store = FragmentStateStore::new();
        store.init("task1", TaskFragmentState::from_plan(8, vec![0, 1]));
        // 覆盖(重试场景)
        store.init("task1", TaskFragmentState::from_plan(16, vec![]));
        let state = store.get("task1").expect("应存在");
        assert_eq!(state.total, 16);
        assert!(state.done_set.is_empty());
    }
}
```

- [ ] **Step 2: 在 mod.rs 导出**

在 `crates/tachyon-app/src/projection/mod.rs` 中追加（或修改现有 pub mod 声明）：

```rust
pub mod fragment_state_store;
pub use fragment_state_store::{FragmentStateStore, TaskFragmentState};
```

- [ ] **Step 3: 运行 store 测试**

Run: `cargo nextest run -p tachyon-app -- test_store --exact`
Expected: 全部 PASS

- [ ] **Step 4: 修改 ChunkReaderJob 加字段 + on_progress 签名**

在 `crates/tachyon-app/src/runtime/chunk_reader_pool.rs` 约 28-38 行的 `ChunkReaderJob` struct，将 `on_progress` 签名改并加 `fragment_state_store` 字段：

```rust
pub struct ChunkReaderJob {
    pub task_id: String,
    pub progress_rx: mpsc::Receiver<tachyon_core::FragmentProgress>,
    pub task_repository: TaskRepository,
    pub task_store: Arc<TaskStore>,
    pub done_tx: oneshot::Sender<()>,
    /// Callback to notify ProgressBroker of progress changes
    /// 第二参数: 新完成分片 index; None = 非完成事件(增量进度)
    pub on_progress: Option<Arc<dyn Fn(&str, Option<u32>) + Send + Sync>>,
    /// 分片状态存储(PlanComplete/Chunk 事件更新)
    pub fragment_state_store: crate::projection::FragmentStateStore,
}
```

- [ ] **Step 5: 修改 run_chunk_reader 的 match 分支**

在 `crates/tachyon-app/src/runtime/chunk_reader_pool.rs` 的 `run_chunk_reader` 函数内，约 194 行的 `while let Some(progress) = progress_rx.recv().await` 循环体，改为 match 两分支。

找到约 194 行：
```rust
    while let Some(progress) = progress_rx.recv().await {
        event_count += 1;
        if progress.completed {
```

替换整个循环体为（保持现有 Chunk 分支内的所有逻辑不变，只包裹进 match）：

```rust
    while let Some(progress) = progress_rx.recv().await {
        match progress {
            tachyon_core::FragmentProgress::PlanComplete {
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
            tachyon_core::FragmentProgress::Chunk {
                fragment_index,
                completed,
                fragment_downloaded,
            } => {
                event_count += 1;
                if completed {
                    completed.insert(fragment_index);
                    pending_completed.push(fragment_index);
                    frag_bytes.remove(&fragment_index);
                    // 更新 FragmentStateStore.done_set
                    fragment_state_store.mark_done(&task_id, fragment_index);
                }
                // --- 以下为现有逻辑(增量更新/速度/TaskInfo回写/checkpoint) ---
                let old = frag_bytes
                    .insert(fragment_index, fragment_downloaded)
                    .unwrap_or(0);
                total_downloaded =
                    total_downloaded.saturating_add(fragment_downloaded.saturating_sub(old));
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
                    task_repository.get(&task_id).map(|t| t.speed).unwrap_or(0)
                };

                {
                    if let Some(mut task) = task_repository.get_mut(&task_id) {
                        task.downloaded = total_downloaded;
                        task.fragments_done = frags_done;
                        task.fragments_total = total_frags;
                        task.speed = speed;
                        if let Some(file_size) = task.file_size.filter(|&s| s > 0) {
                            task.progress = (total_downloaded as f64 / file_size as f64).clamp(0.0, 1.0);
                        } else if total_frags > 0 {
                            task.progress = (frags_done as f64 / total_frags as f64).clamp(0.0, 1.0);
                        }
                    }
                }

                // Notify ProgressBroker of progress changes
                if let Some(ref callback) = on_progress {
                    callback(&task_id, if completed { Some(fragment_index) } else { None });
                }

                // 批量 checkpoint(已完成分片)
                if completed
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

                // 字节级进度 checkpoint(未完整分片)
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
```

注意：`total_frags` 变量需从 immutable 改为 `mut`。找到约 174 行 `let total_frags = ...` 改为 `let mut total_frags = ...`。

- [ ] **Step 6: 编译确认**

Run: `cargo build -p tachyon-app`
Expected: 编译通过（download_session.rs 的 job 构造会报错，Task 4 修复）

- [ ] **Step 7: clippy**

Run: `cargo clippy -p tachyon-app --all-targets -- -D warnings 2>&1 | head -30`
Expected: 可能因 download_session.rs 报错，记录错误待 Task 4 修复

- [ ] **Step 8: 提交**

```bash
git add crates/tachyon-app/src/projection/fragment_state_store.rs crates/tachyon-app/src/projection/mod.rs crates/tachyon-app/src/runtime/chunk_reader_pool.rs
git commit -m "feat(app): FragmentStateStore + chunk reader match PlanComplete/Chunk 两分支"
```

---

## Task 4: ProgressBroker delta 收集 + download_session 构造 + AppState

**Files:**
- Modify: `crates/tachyon-app/src/projection/progress_broker.rs:31-167`
- Modify: `crates/tachyon-app/src/runtime/download_session.rs:269-281`
- Modify: `crates/tachyon-app/src/commands/mod.rs:108-135,210-234,314-318,567-569`
- Modify: `crates/tachyon-app/src/commands/progress_commands.rs:89-110`
- Test: `crates/tachyon-app/src/projection/progress_broker.rs`（内联测试）

**Interfaces:**
- Consumes: `FragmentStateStore`（Task 3）、`ChunkReaderJob` 新字段
- Produces: `TaskProgress` 加 `fragments_total`/`active_concurrency`/`completed_delta` 字段；`TaskInfo` 加 `active_concurrency`；`AppState.fragment_state_store`

- [ ] **Step 1: 写 ProgressBroker delta 测试**

在 `crates/tachyon-app/src/projection/progress_broker.rs` 的 `#[cfg(test)] mod tests` 内追加：

```rust
#[test]
fn test_mark_dirty_with_delta_collects_indices() {
    let repo = crate::repository::TaskRepository::new();
    repo.insert("t1".to_string(), make_test_task_info("t1"));
    let broker = ProgressBroker::new_no_aggregator(repo);
    broker.mark_dirty_with_delta("t1", Some(5));
    broker.mark_dirty_with_delta("t1", Some(8));
    let deltas = broker.pending_deltas.get("t1").unwrap();
    assert_eq!(*deltas, vec![5, 8]);
}

#[test]
fn test_build_progress_event_takes_delta() {
    let repo = crate::repository::TaskRepository::new();
    repo.insert("t1".to_string(), make_test_task_info("t1"));
    let broker = ProgressBroker::new_no_aggregator(repo);
    broker.mark_dirty_with_delta("t1", Some(3));
    let event = build_progress_event(&broker.task_repository, &broker.pending_deltas);
    let tp = event.get("t1").unwrap();
    assert_eq!(tp.completed_delta, vec![3]);
    // take 后清空
    assert!(broker.pending_deltas.get("t1").unwrap().is_empty());
}
```

注：`make_test_task_info` 若不存在，参考现有测试中的辅助函数。`broker.task_repository` 和 `broker.pending_deltas` 需在 struct 内 `pub(crate)` 或在测试模块内可访问（同模块内测试可直接访问私有字段）。

- [ ] **Step 2: 修改 ProgressBroker 结构体加 pending_deltas**

在 `crates/tachyon-app/src/projection/progress_broker.rs` 约 31-41 行的 `ProgressBroker` struct，加字段：

```rust
pub struct ProgressBroker {
    progress_tx: watch::Sender<ProgressEvent>,
    task_repository: TaskRepository,
    aggregator_spawned: AtomicBool,
    dirty_tasks: Arc<DashSet<String>>,
    notify: Arc<Notify>,
    /// 每任务本周期新完成分片索引增量
    pub(crate) pending_deltas: Arc<DashMap<String, Vec<u32>>>,
}
```

在 `start()`（约 49 行）和 `new_no_aggregator()`（约 107 行）的构造中加：

```rust
            pending_deltas: Arc::new(DashMap::new()),
```

- [ ] **Step 3: 新增 mark_dirty_with_delta 方法**

在 `ProgressBroker` impl 块内（`mark_dirty` 方法之后，约 124 行）加：

```rust
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
```

- [ ] **Step 4: 修改 build_progress_event 签名和实现**

在 `crates/tachyon-app/src/projection/progress_broker.rs` 约 148 行，将：

```rust
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
```

替换为：

```rust
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
                    completed_delta,
                },
            )
        })
        .collect()
}
```

- [ ] **Step 5: 修改 spawn_aggregator 和 broadcast_all 的调用**

在 `spawn_aggregator`（约 95 行），将 `let event = build_progress_event(&task_repository_ref);` 改为：

```rust
                let event = build_progress_event(&task_repository_ref, &pending_deltas_ref);
```

并在 spawn 闭包前 clone `pending_deltas`：

```rust
        let dirty_tasks = self.dirty_tasks.clone();
        let notify = self.notify.clone();
        let pending_deltas = self.pending_deltas.clone();  // 新增
```

在 `broadcast_all`（约 130 行），将 `let event = build_progress_event(&self.task_repository);` 改为：

```rust
        let event = build_progress_event(&self.task_repository, &self.pending_deltas);
```

- [ ] **Step 6: 修改 TaskInfo 加 active_concurrency**

在 `crates/tachyon-app/src/commands/mod.rs` 约 108-135 行的 `TaskInfo` struct，在 `fragments_done` 之后加：

```rust
    pub fragments_done: u32,
    /// 当前下载并发度,前端推算 downloading 带宽用
    /// 由 PlanComplete 初始化,运行中不更新(静态初始值)
    #[serde(default)]
    pub active_concurrency: u32,
```

- [ ] **Step 7: 修改 DownloadProgress 和 TaskProgress 加字段**

在 `crates/tachyon-app/src/commands/mod.rs`：

`DownloadProgress`（约 210-221 行）加：

```rust
    pub fragments_done: u32,
    #[serde(default)]
    pub active_concurrency: u32,
```

`TaskProgress`（约 223-232 行）改为：

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskProgress {
    pub id: String,
    pub progress: f64,
    pub speed: u64,
    pub downloaded: u64,
    pub status: DownloadState,
    pub fragments_done: u32,
    #[serde(default)]
    pub fragments_total: u32,
    #[serde(default)]
    pub active_concurrency: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_delta: Vec<u32>,
}
```

- [ ] **Step 8: 修改 AppState 加 fragment_state_store**

在 `crates/tachyon-app/src/commands/mod.rs` 约 314-318 行的 `AppState` struct，加字段：

```rust
pub struct AppState {
    pub(crate) domain: DomainState,
    pub(crate) infra: InfraState,
    pub(crate) service: ServiceState,
    pub(crate) runtime: RuntimeState,
    pub(crate) fragment_state_store: crate::projection::FragmentStateStore,
}
```

在 `AppState::try_new` 和 `AppState::default`/`new` 中初始化 `fragment_state_store: crate::projection::FragmentStateStore::new()`。找到所有构造 AppState 的位置（grep `AppState {` 或 `Self {`），都加上此字段。

- [ ] **Step 9: 修改 cleanup_runtime 加 store 清理**

在 `crates/tachyon-app/src/commands/mod.rs` 约 567-569 行：

```rust
pub(crate) fn cleanup_runtime(state: &AppState, task_id: &str) {
    state.runtime.supervisor.cleanup(task_id);
    state.fragment_state_store.remove(task_id);
}
```

- [ ] **Step 10: 修改 build_initial_progress_event 加字段**

在 `crates/tachyon-app/src/commands/progress_commands.rs` 约 89-110 行，将 TaskProgress 构造改为：

```rust
                TaskProgress {
                    id: id.clone(),
                    progress: t.progress,
                    speed: t.speed,
                    downloaded: t.downloaded,
                    status: t.status,
                    fragments_done: t.fragments_done,
                    fragments_total: t.fragments_total,
                    active_concurrency: t.active_concurrency,
                    completed_delta: vec![],
                },
```

- [ ] **Step 11: 修改 get_download_progress_inner 加字段**

在 `crates/tachyon-app/src/commands/progress_commands.rs` 约 76-86 行，在 DownloadProgress 构造中加 `active_concurrency: task.active_concurrency`。

- [ ] **Step 12: 修改 download_session.rs 的 on_progress 和 job 构造**

在 `crates/tachyon-app/src/runtime/download_session.rs` 约 270-281 行，将：

```rust
        let broker = self.state.runtime.progress_broker.clone();
        let on_progress: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |task_id: &str| {
            broker.mark_dirty(task_id);
        });
        let job = ChunkReaderJob {
            task_id: self.task_id.clone(),
            progress_rx: chunk_progress_rx,
            task_repository: self.state.domain.task_repository.clone(),
            task_store: self.state.infra.task_store.clone(),
            done_tx,
            on_progress: Some(on_progress),
        };
```

替换为：

```rust
        let broker = self.state.runtime.progress_broker.clone();
        let on_progress: Arc<dyn Fn(&str, Option<u32>) + Send + Sync> =
            Arc::new(move |task_id, idx| {
                broker.mark_dirty_with_delta(task_id, idx);
            });
        let job = ChunkReaderJob {
            task_id: self.task_id.clone(),
            progress_rx: chunk_progress_rx,
            task_repository: self.state.domain.task_repository.clone(),
            task_store: self.state.infra.task_store.clone(),
            done_tx,
            on_progress: Some(on_progress),
            fragment_state_store: self.state.fragment_state_store.clone(),
        };
```

- [ ] **Step 13: 修复所有 TaskInfo/TaskProgress 构造点**

grep 全 app crate 找到所有构造 `TaskInfo { ... }` 和 `TaskProgress { ... }` 和 `DownloadProgress { ... }` 的位置，补上新字段。Run: `grep -rn "TaskInfo {" crates/tachyon-app/src/ --include="*.rs"` 和 `grep -rn "TaskProgress {" crates/tachyon-app/src/ --include="*.rs"` 和 `grep -rn "DownloadProgress {" crates/tachyon-app/src/ --include="*.rs"`，每处加 `active_concurrency: 0`（TaskInfo/DownloadProgress）或对应字段。

- [ ] **Step 14: 编译确认**

Run: `cargo build -p tachyon-app`
Expected: 编译通过

- [ ] **Step 15: 运行 broker 测试**

Run: `cargo nextest run -p tachyon-app -- test_mark_dirty_with_delta test_build_progress_event_takes_delta`
Expected: PASS

- [ ] **Step 16: clippy**

Run: `cargo clippy -p tachyon-app --all-targets --all-features -- -D warnings 2>&1 | head -30`
Expected: 零警告

- [ ] **Step 17: 提交**

```bash
git add crates/tachyon-app/src/projection/progress_broker.rs crates/tachyon-app/src/runtime/download_session.rs crates/tachyon-app/src/commands/mod.rs crates/tachyon-app/src/commands/progress_commands.rs
git commit -m "feat(app): ProgressBroker delta 收集 + TaskInfo/TaskProgress 加字段 + AppState 加 store"
```

---

## Task 5: get_task_fragments command + 注册

**Files:**
- Create: `crates/tachyon-app/src/commands/fragment_commands.rs`
- Modify: `crates/tachyon-app/src/commands/mod.rs`（导出）
- Modify: `crates/tachyon-app/src/lib.rs:130-166`（注册）
- Test: `crates/tachyon-app/src/commands/fragment_commands.rs`（内联测试）

**Interfaces:**
- Consumes: `FragmentStateStore`（Task 3）、`AppState`（Task 4）
- Produces: `get_task_fragments` Tauri command + `TaskFragmentsView` 类型

- [ ] **Step 1: 创建 fragment_commands.rs 并写测试**

创建 `crates/tachyon-app/src/commands/fragment_commands.rs`：

```rust
//! 分片查询命令

use super::{AppError, AppState};
use serde::Serialize;

/// get_task_fragments 返回视图
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskFragmentsView {
    /// 真实分片总数
    pub total: u32,
    /// 已完成分片索引列表
    pub done_indices: Vec<u32>,
}

/// 查询任务分片状态(DetailPanel 打开时调用)
#[tauri::command]
pub async fn get_task_fragments(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<TaskFragmentsView, AppError> {
    let Some(frag_state) = state.fragment_state_store.get(&task_id) else {
        return Ok(TaskFragmentsView {
            total: 0,
            done_indices: vec![],
        });
    };
    Ok(TaskFragmentsView {
        total: frag_state.total,
        done_indices: frag_state.done_set.iter().copied().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::{FragmentStateStore, TaskFragmentState};

    #[test]
    fn test_task_fragments_view_from_empty_store() {
        let store = FragmentStateStore::new();
        // 模拟 command 逻辑(无 AppState 时直接测 store)
        let result = store.get("nonexistent");
        assert!(result.is_none());
    }

    #[test]
    fn test_task_fragments_view_from_initialized_store() {
        let store = FragmentStateStore::new();
        store.init(
            "t1",
            TaskFragmentState::from_plan(8, vec![0, 1, 2]),
        );
        let frag_state = store.get("t1").expect("应存在");
        assert_eq!(frag_state.total, 8);
        let done_indices: Vec<u32> = frag_state.done_set.iter().copied().collect();
        assert_eq!(done_indices, vec![0, 1, 2]);
    }
}
```

- [ ] **Step 2: 在 mod.rs 导出**

在 `crates/tachyon-app/src/commands/mod.rs` 的模块声明区（文件顶部附近）加：

```rust
pub mod fragment_commands;
pub use fragment_commands::{get_task_fragments, TaskFragmentsView};
```

- [ ] **Step 3: 注册到 invoke_handler**

在 `crates/tachyon-app/src/lib.rs` 约 146-147 行（`subscribe_progress` 之后）加：

```rust
            subscribe_progress,
            get_task_fragments,
```

- [ ] **Step 4: 编译确认**

Run: `cargo build -p tachyon-app`
Expected: 编译通过

- [ ] **Step 5: 运行测试**

Run: `cargo nextest run -p tachyon-app -- test_task_fragments_view`
Expected: PASS

- [ ] **Step 6: clippy**

Run: `cargo clippy -p tachyon-app --all-targets --all-features -- -D warnings 2>&1 | head -20`
Expected: 零警告

- [ ] **Step 7: 提交**

```bash
git add crates/tachyon-app/src/commands/fragment_commands.rs crates/tachyon-app/src/commands/mod.rs crates/tachyon-app/src/lib.rs
git commit -m "feat(app): get_task_fragments command + TaskFragmentsView"
```

---

## Task 6: 全后端集成验证

**Files:**
- 无新增修改，仅验证

- [ ] **Step 1: 全量编译**

Run: `cargo build --all`
Expected: 编译通过

- [ ] **Step 2: 全量测试**

Run: `cargo nextest run --all`
Expected: 全部通过（修复因 FragmentProgress 枚举化导致的 break）

- [ ] **Step 3: clippy 全量**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: 零警告

- [ ] **Step 4: fmt 检查**

Run: `cargo fmt --all -- --check`
Expected: 无差异（如有，运行 `cargo fmt --all` 后重新检查）

- [ ] **Step 5: 如有 fmt 差异，修复并提交**

```bash
cargo fmt --all
git add -A
git commit -m "style: 格式化"
```

---

## Task 7: 前端类型 + API + taskFragments store

**Files:**
- Modify: `frontend/src/types.ts:183-192`
- Modify: `frontend/src/api/invoke.ts:127-129`
- Create: `frontend/src/stores/taskFragments.ts`
- Test: `frontend/src/stores/taskFragments.ts`（内联或 spec 文件）

**Interfaces:**
- Consumes: 后端 `TaskFragmentsView`、`ProgressPayload` 新字段
- Produces: `loadTaskFragments`/`mergeFragmentDelta`/`getTaskFragmentData`/`clearTaskFragments`

- [ ] **Step 1: 修改 types.ts**

在 `frontend/src/types.ts` 约 183 行的 `ProgressPayload`，改为：

```ts
export interface ProgressPayload {
  id: string
  progress: number
  downloaded: number
  speed: number
  status: DownloadStatus
  fragmentsDone: number
  fragmentsTotal: number
  activeConcurrency: number
  completedDelta?: number[]
}
```

在文件末尾（或合适位置）加：

```ts
/** get_task_fragments 返回 */
export interface TaskFragmentsView {
  total: number
  doneIndices: number[]
}
```

- [ ] **Step 2: 修改 invoke.ts**

在 `frontend/src/api/invoke.ts` 约 129 行 `subscribeProgress` 之后加：

```ts
  /** 获取任务分片状态(DetailPanel 打开时调用) */
  getTaskFragments: (taskId: string) => invoke<TaskFragmentsView>('get_task_fragments', { taskId }),
```

确认 invoke.ts 顶部的 import 包含 `TaskFragmentsView` 类型。

- [ ] **Step 3: 创建 taskFragments.ts store**

创建 `frontend/src/stores/taskFragments.ts`：

```ts
import { createSignal } from "solid-js";
import { api } from "../api/invoke";

// 单任务分片数据:真实 doneSet + 并发度
interface TaskFragmentData {
  total: number;
  concurrency: number;
  doneSet: Set<number>;
}

const [fragmentMap, setFragmentMap] = createSignal<Map<string, TaskFragmentData>>(
  new Map(),
);

// 竞态防护 token:DetailPanel task 切换时,旧的 loadTaskFragments 返回被丢弃
let currentLoadToken = 0;

/** DetailPanel 打开/task 切换时调用:首拉元数据 + 初始 doneSet */
export async function loadTaskFragments(taskId: string) {
  const token = ++currentLoadToken;
  const view = await api.getTaskFragments(taskId);
  if (token !== currentLoadToken) return; // 已被后续切换覆盖,丢弃
  const doneSet = new Set<number>(view.doneIndices);
  setFragmentMap((prev) => {
    const next = new Map(prev);
    next.set(taskId, { total: view.total, concurrency: 0, doneSet });
    return next;
  });
}

/** DetailPanel 关闭时调用:清理 */
export function clearTaskFragments(taskId: string) {
  setFragmentMap((prev) => {
    const next = new Map(prev);
    next.delete(taskId);
    return next;
  });
}

/** updateProgress 调用:合并 delta + 更新 concurrency */
export function mergeFragmentDelta(
  taskId: string,
  delta: number[],
  concurrency: number,
) {
  setFragmentMap((prev) => {
    const data = prev.get(taskId);
    if (!data) return prev; // DetailPanel 未打开,忽略(后续首拉拿完整 doneSet)
    const next = new Map(prev);
    const newSet = new Set(data.doneSet);
    for (const idx of delta) newSet.add(idx);
    next.set(taskId, {
      ...data,
      doneSet: newSet,
      concurrency: concurrency || data.concurrency,
    });
    return next;
  });
}

/** ChunkMatrix 读取:获取任务分片数据 */
export function getTaskFragmentData(taskId: string) {
  return fragmentMap().get(taskId);
}
```

- [ ] **Step 4: 类型检查**

Run: `cd frontend && bun run tsc --noEmit 2>&1 | head -20`
Expected: 无类型错误（如有，修复 import 缺失等）

- [ ] **Step 5: 提交**

```bash
cd frontend
git add src/types.ts src/api/invoke.ts src/stores/taskFragments.ts
git commit -m "feat(frontend): TaskFragmentsView 类型 + getTaskFragments API + taskFragments store"
```

---

## Task 8: updateProgress 合并 delta + fragmentsTotal

**Files:**
- Modify: `frontend/src/stores/downloads.ts:192-289`

**Interfaces:**
- Consumes: `mergeFragmentDelta`/`getTaskFragmentData`/`loadTaskFragments`（Task 7）、`ProgressPayload` 新字段

- [ ] **Step 1: 在 downloads.ts 顶部加 import**

在 `frontend/src/stores/downloads.ts` 的 import 区（约第 12 行后）加：

```ts
import { mergeFragmentDelta, getTaskFragmentData, loadTaskFragments } from "./taskFragments";
```

- [ ] **Step 2: 修改 updateProgress 函数**

在 `frontend/src/stores/downloads.ts` 的 `updateProgress` 函数内（约 192-289 行），找到现有字段解析处（约 213-216 行）：

```ts
      const newDownloaded = p.downloaded ?? task.downloaded;
      const newSpeed = p.speed ?? task.speed;
      const newProgress = p.progress ?? task.progress;
      const newFragmentsDone = p.fragmentsDone ?? task.fragmentsDone;
```

在之后加：

```ts
      const newFragmentsTotal = p.fragmentsTotal ?? task.fragmentsTotal;
      const newConcurrency = p.activeConcurrency ?? 0;
```

在 cold 层更新处（约 244-252 行的 `setTasksRaw` 调用），加 `fragmentsTotal`：

```ts
        if (hasChanged) {
          setTasksRaw(idx, {
            downloaded: newDownloaded,
            speed: newSpeed,
            status: newStatus,
            progress: newProgress,
            fragmentsDone: newFragmentsDone,
            fragmentsTotal: newFragmentsTotal,
          });
        }
```

在 `hasChanged` 块之后、状态转 terminal 判断之前，加分片 delta 合并：

```ts
        // 合并分片 delta 到 fragment store
        if (p.completedDelta && p.completedDelta.length > 0) {
          mergeFragmentDelta(id, p.completedDelta, newConcurrency);
        } else if (newConcurrency > 0) {
          mergeFragmentDelta(id, [], newConcurrency);
        }

        // fragmentsTotal 从 0 变非 0:PlanComplete 到达,DetailPanel 若已打开需重拉
        if (
          task.fragmentsTotal === 0 &&
          newFragmentsTotal > 0 &&
          getTaskFragmentData(id) === undefined
        ) {
          loadTaskFragments(id);
        }
```

- [ ] **Step 3: 类型检查**

Run: `cd frontend && bun run tsc --noEmit 2>&1 | head -20`
Expected: 无类型错误

- [ ] **Step 4: 运行现有前端测试确认无回归**

Run: `cd frontend && bun test 2>&1 | tail -20`
Expected: 现有测试通过

- [ ] **Step 5: 提交**

```bash
cd frontend
git add src/stores/downloads.ts
git commit -m "feat(frontend): updateProgress 合并 completedDelta + fragmentsTotal 同步"
```

---

## Task 9: ChunkMatrix DOM + Canvas 双模式改读 doneSet

**Files:**
- Modify: `frontend/src/components/ChunkMatrix.tsx:15-19`（Props）
- Modify: `frontend/src/components/ChunkMatrix.tsx:60-103`（buildBlocks）
- Modify: `frontend/src/components/ChunkMatrix.tsx:249-275`（chunks/blocks memo）

**Interfaces:**
- Consumes: `getTaskFragmentData`/`mergeFragmentDelta`（Task 7）

- [ ] **Step 1: 修改 Props 接口加 taskId**

在 `frontend/src/components/ChunkMatrix.tsx` 约 15-19 行：

```ts
interface ChunkMatrixProps {
  taskId: string;
  fragmentsTotal: number;
  fragmentsDone: number;
  progress: number;
}
```

- [ ] **Step 2: 加 import**

在文件顶部 import 区加：

```ts
import { getTaskFragmentData, mergeFragmentDelta } from "../stores/taskFragments";
```

- [ ] **Step 3: 修改 buildBlocks 接收 doneSet + concurrency**

将约 60-103 行的 `buildBlocks` 函数签名和实现改为：

```ts
export function buildBlocks(
  total: number,
  done: number,
  progress: number,
  doneSet: Set<number>,
  concurrency: number,
): ChunkBlock[] {
  if (total <= 0) return [];
  const blockCount = Math.min(total, AGGREGATE_BLOCKS);
  const maxDoneIdx = doneSet.size > 0 ? Math.max(...doneSet) : -1;
  const remaining = total - done;
  const band = Math.min(Math.max(1, concurrency), Math.max(1, remaining));
  const blocks: ChunkBlock[] = [];
  for (let i = 0; i < blockCount; i++) {
    const start = Math.floor((i * total) / blockCount);
    const end = Math.max(start + 1, Math.floor(((i + 1) * total) / blockCount));
    const blockTotal = end - start;
    let blockDone = 0;
    let blockDownloading = 0;
    for (let f = start; f < end; f++) {
      if (doneSet.has(f)) {
        blockDone++;
      } else if (f > maxDoneIdx && f <= maxDoneIdx + band && progress < 1) {
        blockDownloading++;
      }
    }
    const blockPending = blockTotal - blockDone - blockDownloading;
    let status: ChunkBlock["status"];
    if (blockDone >= blockDownloading && blockDone >= blockPending) {
      status = "done";
    } else if (blockDownloading >= blockPending) {
      status = "downloading";
    } else {
      status = "pending";
    }
    blocks.push({
      index: i,
      start,
      end,
      done: blockDone,
      total: blockTotal,
      status,
      color: STATUS_COLOR_VARS[status],
    });
  }
  return blocks;
}
```

- [ ] **Step 4: 修改 chunks memo 读真实 doneSet**

将约 249-271 行的 `chunks` memo 改为：

```ts
  const fragData = createMemo(() => getTaskFragmentData(props.taskId));

  const chunks = createMemo(() => {
    if (props.fragmentsTotal > AGGREGATE_THRESHOLD) return [];
    const data = fragData();
    // 无 store 数据时回退到旧推算(store 未加载完成期间)
    const doneSet = data?.doneSet ?? new Set<number>();
    const concurrency = data?.concurrency || Math.max(2, Math.round(props.fragmentsTotal / 8));
    const done = props.fragmentsDone;
    const maxDoneIdx = doneSet.size > 0 ? Math.max(...doneSet) : -1;
    const remaining = props.fragmentsTotal - done;
    const band = Math.min(Math.max(1, concurrency), Math.max(1, remaining));
    return Array.from({ length: props.fragmentsTotal }, (_, i) => {
      const isDone = doneSet.has(i);
      // 已知折中:maxDoneIdx 之前未完成的分片(如重试中)显示为 pending
      const isDownloading =
        !isDone && i > maxDoneIdx && i <= maxDoneIdx + band && props.progress < 1;
      const status: ChunkBlock["status"] = isDone
        ? "done"
        : isDownloading
          ? "downloading"
          : "pending";
      return {
        index: i,
        isDone,
        isDownloading,
        color: STATUS_COLOR_VARS[status],
      };
    });
  });
```

- [ ] **Step 5: 修改 blocks memo 调用**

将约 273-275 行的 `blocks` memo 改为：

```ts
  const blocks = createMemo(() => {
    const data = fragData();
    const doneSet = data?.doneSet ?? new Set<number>();
    const concurrency = data?.concurrency || Math.max(2, Math.round(props.fragmentsTotal / 8));
    return buildBlocks(props.fragmentsTotal, props.fragmentsDone, props.progress, doneSet, concurrency);
  });
```

- [ ] **Step 6: 加整块下载兜底 effect**

在 `chunks` memo 之后加：

```ts
  // 整块下载兜底:任务完成但 doneSet 为空(单分片整块下载无 Chunk::completed 事件)
  createEffect(() => {
    if (props.progress >= 1 && fragData() && fragData()!.doneSet.size === 0) {
      mergeFragmentDelta(props.taskId, [0], 0);
    }
  });
```

- [ ] **Step 7: 类型检查**

Run: `cd frontend && bun run tsc --noEmit 2>&1 | head -20`
Expected: 无类型错误（DetailPanel 传 props 处可能报缺 taskId，Task 10 修复）

- [ ] **Step 8: 运行 ChunkMatrix 测试**

Run: `cd frontend && bun test -- ChunkMatrix 2>&1 | tail -20`
Expected: 现有测试可能因 buildBlocks 签名变化失败，更新测试（见 Step 9）

- [ ] **Step 9: 更新 ChunkMatrix spec**

在 `frontend/src/components/__tests__/ChunkMatrix.spec.tsx` 中，更新所有 `buildBlocks` 调用加 `doneSet` + `concurrency` 参数，以及 ChunkMatrix 组件调用加 `taskId` prop。例如：

```ts
// 旧: buildBlocks(100, 50, 0.5)
// 新:
buildBlocks(100, 50, 0.5, new Set(Array.from({length: 50}, (_, i) => i)), 4)
```

ChunkMatrix 组件测试加 `taskId="test-task"` prop。如需 mock taskFragments store，用 `vi.mock("../stores/taskFragments")` 或直接在测试中通过 store API 初始化。

- [ ] **Step 10: 提交**

```bash
cd frontend
git add src/components/ChunkMatrix.tsx src/components/__tests__/ChunkMatrix.spec.tsx
git commit -m "feat(frontend): ChunkMatrix DOM+Canvas 双模式读真实 doneSet"
```

---

## Task 10: DetailPanel 生命周期 + props 接入

**Files:**
- Modify: `frontend/src/components/DetailPanel.tsx:56`（import）
- Modify: `frontend/src/components/DetailPanel.tsx:68-71`（Props 不变，但需传 taskId）
- Modify: `frontend/src/components/DetailPanel.tsx:903-917`（ChunkMatrix 调用）
- Modify: `frontend/src/components/DetailPanel.tsx:104-131`（task 切换 effect）

**Interfaces:**
- Consumes: `loadTaskFragments`/`clearTaskFragments`（Task 7）

- [ ] **Step 1: 加 import**

在 `frontend/src/components/DetailPanel.tsx` 顶部 import 区加：

```ts
import { loadTaskFragments, clearTaskFragments, getTaskFragmentData } from "../stores/taskFragments";
```

- [ ] **Step 2: 修改 ChunkMatrix 调用加 taskId**

在约 910-914 行的 `<ChunkMatrix>` 调用，加 `taskId` prop：

```tsx
                <ChunkMatrix
                  taskId={task()!.id}
                  fragmentsTotal={task()!.fragmentsTotal}
                  fragmentsDone={task()!.fragmentsDone}
                  progress={task()!.progress}
                />
```

- [ ] **Step 3: 加 task 切换时加载分片数据**

在约 104-131 行的 `createEffect`（监听 `props.task`）内，在 `setDisplayTask(task)` 之后加分片加载逻辑。或在组件内新增一个 effect：

```ts
  // task 变化时按需加载分片数据(DetailPanel 打开/task 切换)
  createEffect(() => {
    const task = props.task;
    if (!task) return;
    if (getTaskFragmentData(task.id)) return; // 已有数据,不重复拉
    loadTaskFragments(task.id);
  });
```

- [ ] **Step 4: 加 onCleanup 清理**

在组件内（onMount 或组件体顶层）加：

```ts
  onCleanup(() => {
    const task = props.task;
    if (task) clearTaskFragments(task.id);
  });
```

确认 `onCleanup` 已在顶部 import 中（如未 import，加到 `solid-js` 的 import）。

- [ ] **Step 5: 类型检查**

Run: `cd frontend && bun run tsc --noEmit 2>&1 | head -20`
Expected: 无类型错误

- [ ] **Step 6: 运行前端测试**

Run: `cd frontend && bun test 2>&1 | tail -20`
Expected: 全部通过

- [ ] **Step 7: 提交**

```bash
cd frontend
git add src/components/DetailPanel.tsx
git commit -m "feat(frontend): DetailPanel 生命周期加载/清理分片数据 + ChunkMatrix 传 taskId"
```

---

## Task 11: 全前端集成验证

**Files:**
- 无新增修改，仅验证

- [ ] **Step 1: 类型检查**

Run: `cd frontend && bun run tsc --noEmit`
Expected: 无错误

- [ ] **Step 2: 全量测试**

Run: `cd frontend && bun test`
Expected: 全部通过

- [ ] **Step 3: 手动验证（如有 dev 环境）**

Run: `cargo tauri dev`
验证点：
1. 创建下载任务，观察 ChunkMatrix 显示真实分片数（非 1MB 估算）
2. 下载中观察 done 集合增量更新（非顺序推算）
3. 暂停后恢复，观察续传已完成分片正确显示为 done
4. 大文件（>200 分片）观察 Canvas 模式渲染正确
5. 快速切换 DetailPanel 不同任务，无竞态错误

- [ ] **Step 4: 全后端 CI 预检**

Run: `cargo fmt --all -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all`
Expected: 全部通过

- [ ] **Step 5: 最终提交（如有遗留改动）**

```bash
git add -A
git commit -m "test: 全前端集成验证通过"
```

---

## Self-Review 检查记录

**Spec coverage:** 对照设计文档逐节检查——FragmentProgress 枚举化(Task 1)、engine 3 send 点+plan send(Task 2)、FragmentStateStore+chunk reader match(Task 3)、broker delta+AppState+TaskInfo/TaskProgress 加字段(Task 4)、get_task_fragments command(Task 5)、前端类型+API+store(Task 7)、updateProgress(Task 8)、ChunkMatrix 双模式(Task 9)、DetailPanel(Task 10)。全覆盖。

**Type consistency:** `FragmentProgress::PlanComplete{total,completed_indices,initial_concurrency}` 在 Task 1 定义、Task 2 发送、Task 3 消费——字段名一致。`TaskProgress{fragments_total,active_concurrency,completed_delta}` 在 Task 4 定义、Task 8 前端消费——camelCase 转换后 `fragmentsTotal`/`activeConcurrency`/`completedDelta` 一致。`TaskFragmentsView{total,done_indices}` → 前端 `{total,doneIndices}` 一致。

**已知限制已记录在 spec 中，不在计划中重复修复。**
