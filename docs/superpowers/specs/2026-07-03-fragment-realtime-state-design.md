# 分片真实状态前端对接设计

## 背景

当前 Tachyon 的分片（fragment）下载逻辑在后端相当完整——Holt 带宽预测调度、dispatcher/worker 双通道并发、字节级断点续传、流式哈希校验。但前后端契约停留在「聚合标量」层，存在两个问题：

1. **`fragments_total` 是 probe 阶段的 1MB 硬估算值**（`task_commands.rs:397-401`：`file_size.div_ceil(1024*1024)`），不是 `plan_fragments` 实际生成的分片数。调度器建议的分片大小与 1MB 无关，两者可能差数倍。

2. **前端 ChunkMatrix 的逐分片状态是纯推算**（`ChunkMatrix.tsx:249-271`）：用 `i < done` 判 done，`done + band`（band=total/8）判 downloading，完全不依赖后端逐分片真实状态。

## 目标

- **对齐 `fragments_total`**：前端拿到 `plan_fragments` 的真实分片数，覆盖 probe 估算值。
- **传逐分片状态**：让 ChunkMatrix 显示真实 done 集合，而非顺序推算。

## 方案选择

### 子问题 1：app 层如何拿到 plan 结果

经全方案评估（详见「备选方案」节），选择 **1A：PlanComplete 事件**。

`plan()` 在 `DownloadTask::run_inner()`（`downloader.rs:2060`）内被调用，返回值被丢弃（`self.plan()?;` 不接收）。app 层无法拦截其返回值。但 `progress_tx` 在 `run()` 之前已 `set_progress_sender`（`download_session.rs:265-267`），plan() 执行时通道已就绪。因此在 plan() 末尾通过已有 progress_tx 发 PlanComplete 事件，是最小侵入的方案。

### 子问题 2：前端如何拿到运行中分片状态

经深度反思，选择 **2B'：PlanComplete 带 initial_concurrency + 增量 done 数组 + 前端推算 downloading 带宽**。

放弃的方案及原因：
- **2A（纯轮询）**：2 秒延迟导致 DetailPanel 打开时视觉跳变。
- **2F（StateChange 逐片状态）**：需把 `FragmentState` 移到 core crate + worker spawn 内加 send 点，且 StateChange 有通道丢失风险需 5s 校正轮询。代价远超收益。
- **2B（纯增量 done 数组，无 concurrency）**：downloading 推算用 `total - done`，大文件下 downloading 区域过大。

2B' 的 downloading 推算是已知折中：用 `min(concurrency, remaining)` 从 done 边界后推，正常顺序下载时准确；乱序/重试场景下，maxDoneIdx 之前的未完成分片会显示为 pending（详见「已知限制」节）。

### 关键简化

设计过程中发现：**前端 ChunkMatrix 完全不需要分片元数据（start/end/size/downloaded/hash）**。tooltip（`ChunkMatrix.tsx:508-555`）只展示分片索引和状态计数，不展示字节范围。因此：

- PlanComplete 只传 `total: u32` + `completed_indices: Vec<u32>`，不传 `Vec<FragmentInfo>`
- get_task_fragments 只返回 `{ total, done_indices }`，不返回分片元数据
- payload 从 MB 级（万级 FragmentInfo）降到 KB 级（索引数组）
- plan() 末尾零 clone

## 数据结构定义

### tachyon-core 层

**`FragmentProgress` 枚举化**（`tachyon-core/src/types.rs:359`，替换原 Copy struct）

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum FragmentProgress {
    /// plan 完成：携带真实分片总数 + 续传已完成索引 + 初始并发度
    /// 仅 plan() 末尾发送一次，用 send().await（此时 channel 必空，不阻塞）
    PlanComplete {
        total: u32,
        completed_indices: Vec<u32>,
        initial_concurrency: u32,
    },
    /// 分片下载进度（原 struct 三字段，语义不变）
    /// 增量用 try_send（可丢），完成用 send().await
    Chunk {
        fragment_index: u32,
        completed: bool,
        fragment_downloaded: u64,
    },
}
```

失去 `Copy`（含 Vec）。消费点仅 `run_chunk_reader` 一处，`match progress`（消费）即可。不引入 `FragmentState` 到 core——用 `completed_indices: Vec<u32>` 表达续传初始态，绕开 crate 依赖（FragmentState 在 engine，FragmentProgress 在 core）。

### tachyon-app 层

**`TaskInfo` 加字段**（`commands/mod.rs:108`）

```rust
pub struct TaskInfo {
    // ... 现有字段不变 ...
    pub fragments_total: u32,   // 已存在，PlanComplete 覆盖 probe 估算值
    pub fragments_done: u32,    // 已存在
    /// 新增：当前下载并发度，前端推算 downloading 带宽用
    /// 由 PlanComplete 初始化，运行中不更新（静态初始值）
    #[serde(default)]
    pub active_concurrency: u32,
}
```

**`TaskProgress` 加字段**（`commands/mod.rs:225`，progress-update 事件 payload）

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
    /// 新增：真实分片总数（覆盖前端旧估算值）
    #[serde(default)]
    pub fragments_total: u32,
    /// 新增：当前并发度，前端推算 downloading 带宽
    #[serde(default)]
    pub active_concurrency: u32,
    /// 新增：本周期新完成分片索引（前端 add 到 doneSet）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_delta: Vec<u32>,
}
```

`DownloadProgress`（`mod.rs:210`，get_download_progress 返回）同步加 `active_concurrency: u32`。

**`TaskFragmentState` + `FragmentStateStore`**（新增，`tachyon-app/src/projection/fragment_state_store.rs`）

```rust
/// 单个任务的分片运行时状态（内存，随任务生命周期）
pub struct TaskFragmentState {
    pub total: u32,
    pub done_set: BTreeSet<u32>,
}

impl TaskFragmentState {
    pub fn from_plan(total: u32, completed_indices: Vec<u32>) -> Self {
        Self {
            total,
            done_set: completed_indices.into_iter().collect(),
        }
    }
    pub fn mark_done(&mut self, index: u32) {
        self.done_set.insert(index);
    }
}

/// 全局分片状态存储，长存于 AppState
#[derive(Clone, Default)]
pub struct FragmentStateStore(Arc<DashMap<String, TaskFragmentState>>);

impl FragmentStateStore {
    pub fn new() -> Self { Self::default() }
    pub fn init(&self, task_id: &str, state: TaskFragmentState) { self.0.insert(task_id.to_string(), state); }
    pub fn mark_done(&self, task_id: &str, index: u32) {
        if let Some(mut state) = self.0.get_mut(task_id) { state.mark_done(index); }
    }
    pub fn get(&self, task_id: &str) -> Option<Ref<'_, String, TaskFragmentState>> { self.0.get(task_id) }
    pub fn remove(&self, task_id: &str) { self.0.remove(task_id); }
}
```

**`TaskFragmentsView`**（get_task_fragments 返回）

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskFragmentsView {
    pub total: u32,
    pub done_indices: Vec<u32>,
}
```

### 前端 TypeScript 类型

```ts
// types.ts 新增
export interface TaskFragmentsView {
  total: number
  doneIndices: number[]
}

// types.ts 修改 ProgressPayload
export interface ProgressPayload {
  id: string
  progress: number
  downloaded: number
  speed: number
  status: DownloadStatus
  fragmentsDone: number
  fragmentsTotal: number       // 新增
  activeConcurrency: number    // 新增
  completedDelta?: number[]    // 新增
}
```

## 数据流

```
tachyon-engine: plan() 末尾
  ├─ total = self.fragments.len() as u32
  ├─ completed_indices = self.fragments.iter().filter(Done).map(index).collect()
  ├─ initial_concurrency = recommendation.concurrency
  └─ progress_tx.send(PlanComplete{total, completed_indices, initial_concurrency}).await
     （channel 必空，不阻塞；FIFO 保证先于 Chunk）

tachyon-engine: execute_fragmented_download worker
  └─ try_send(Chunk{completed:false,..}) 每5chunk
  └─ send(Chunk{completed:true,..}).await 分片完成

         │ FragmentProgress (enum)
         ▼

tachyon-app: run_chunk_reader (match 两分支)
  ├─ PlanComplete → 覆盖 total_frags + 回写 TaskInfo{fragments_total, active_concurrency}
  │                 + 初始化 FragmentStateStore{total, done_set=completed_indices}
  │                 + 初始化 completed BTreeSet（续传 done 集）
  ├─ Chunk{completed:true} → done_set.insert + completed.insert
  │                          + on_progress(task_id, Some(idx)) → broker.pending_deltas.push
  └─ Chunk{completed:false} → frag_bytes 更新 + on_progress(task_id, None)

         │
         ▼

tachyon-app: ProgressBroker 250ms tick
  └─ build_progress_event(task_repo, pending_deltas)
     → take pending_deltas 放入 TaskProgress.completed_delta
     → TaskProgress{fragmentsTotal, activeConcurrency, completedDelta}
         │
         ▼

subscribe_progress → compute_progress_delta → emit("progress-update", delta)

         │
         ▼

前端: updateProgress
  ├─ cold 层: setTasksRaw(fragmentsTotal, activeConcurrency)
  └─ mergeFragmentDelta(id, completedDelta, activeConcurrency) → taskFragments store

前端: DetailPanel 打开 → invoke get_task_fragments → {total, doneIndices}
  └─ taskFragments store 初始化 {total, doneSet, concurrency}

前端: ChunkMatrix 读 taskFragments store
  ├─ DOM 模式 (≤200): doneSet.has(i) + min(concurrency,remaining) 推算 downloading
  └─ Canvas 模式 (>200): buildBlocks 接收 doneSet，按区间统计 done 比例
```

## engine 侧改动

### 改动 1：FragmentProgress 枚举化

文件：`crates/tachyon-core/src/types.rs:359-367`

删除原 `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct FragmentProgress`，替换为上文定义的 `enum FragmentProgress`。失去 `Copy` + `Eq`，改为 `Clone, PartialEq`。

### 改动 2：report_progress 改构造

文件：`crates/tachyon-engine/src/downloader.rs:1634-1653`

签名不变（`&Option<Sender<FragmentProgress>>`），构造体改为 `FragmentProgress::Chunk { fragment_index, completed: false, fragment_downloaded }`。

### 改动 3：分片完成 send 改构造

文件：`crates/tachyon-engine/src/downloader.rs:1860-1867`

`tx.send(FragmentProgress::Chunk { fragment_index, completed: true, fragment_downloaded }).await`。

### 改动 4：plan() 末尾发 PlanComplete

文件：`crates/tachyon-engine/src/downloader.rs:852-854`（`Ok(fragments)` 之前插入）

```rust
if let Some(tx) = &self.progress_tx {
    let total = self.fragments.len() as u32;
    let completed_indices: Vec<u32> = self.fragments.iter()
        .filter(|f| f.state == crate::fragment::FragmentState::Done)
        .map(|f| f.info.index)
        .collect();
    if let Err(e) = tx.send(FragmentProgress::PlanComplete {
        total,
        completed_indices,
        initial_concurrency: recommendation.concurrency,
    }).await {
        warn!(error = %e, "PlanComplete 事件发送失败");
    }
}
Ok(fragments)
```

关键点：
- `recommendation` 变量在 plan() 作用域内（786-788 定义），852 行可访问。`recommendation.concurrency` 的类型取决于 `ScheduleRecommendation` 结构体定义——实现时需确认，若非 u32 则 `as u32` 转换。
- `self.fragments` 在 815-818 赋值，820-852 续传标记已执行。Done 状态来自续传恢复（829 行 `complete_download_fast`）。非续传场景为空 Vec。
- 零 clone：只取 `len()` 和索引，不 clone `Vec<FragmentInfo>`。
- PlanComplete 一定先于 Chunk：channel 是 mpsc FIFO，plan() 在 `run_inner()` 2060 行执行，execute() 在 2082+ 行。

### 整块下载兜底

`execute_full_download`（`downloader.rs:918`）不发任何 Chunk 事件。PlanComplete 仍会发（plan() 对单分片返回 `vec![单个 FragmentInfo]`），前端拿到 total=1。done 的兜底在前端：任务进入 Completed 终态时，progress-update 的 `status: Completed` 触发前端把单分片标 done（`progress >= 1 && doneSet.size === 0` 时 `mergeFragmentDelta(id, [0], 0)`）。

### engine 侧零结构体字段变更、零 trait 契约变更、零新增依赖。

## app 侧改动

### 改动 1：run_chunk_reader 扩展 match 分支

文件：`crates/tachyon-app/src/runtime/chunk_reader_pool.rs:161-342`

`total_frags`（174-177）从 immutable 改为 `mut`，PlanComplete 到达时覆盖。`completed` BTreeSet 在 PlanComplete 时初始化为续传 done 集。核心结构：

```rust
while let Some(progress) = progress_rx.recv().await {
    match progress {
        FragmentProgress::PlanComplete { total, completed_indices, initial_concurrency } => {
            total_frags = total;
            if let Some(mut task) = task_repository.get_mut(&task_id) {
                task.fragments_total = total;
                task.active_concurrency = initial_concurrency;
            }
            fragment_state_store.init(&task_id, TaskFragmentState::from_plan(total, completed_indices.clone()));
            completed = completed_indices.into_iter().collect();
            if let Some(ref callback) = on_progress { callback(&task_id, None); }
        }
        FragmentProgress::Chunk { fragment_index, completed, fragment_downloaded } => {
            // 现有逻辑（增量更新/速度/TaskInfo回写/checkpoint）不变
            // on_progress callback 第二参数: Some(fragment_index) if completed else None
            // 新增: completed 时 fragment_state_store.mark_done(&task_id, fragment_index)
        }
    }
}
```

Chunk 分支内现有逻辑（196-312）保持不变，仅 `on_progress` 调用改为传第二参数，`completed` 时额外调 `fragment_state_store.mark_done`。

### 改动 2：ChunkReaderJob 加字段 + on_progress 签名改

文件：`crates/tachyon-app/src/runtime/chunk_reader_pool.rs:28-38`

```rust
pub struct ChunkReaderJob {
    pub task_id: String,
    pub progress_rx: mpsc::Receiver<FragmentProgress>,
    pub task_repository: TaskRepository,
    pub task_store: Arc<TaskStore>,
    pub done_tx: oneshot::Sender<()>,
    pub on_progress: Option<Arc<dyn Fn(&str, Option<u32>) + Send + Sync>>,  // 签名改
    pub fragment_state_store: FragmentStateStore,  // 新增
}
```

### 改动 3：构造点改

文件：`crates/tachyon-app/src/runtime/download_session.rs:270-281`

```rust
let broker = self.state.runtime.progress_broker.clone();
let on_progress: Arc<dyn Fn(&str, Option<u32>) + Send + Sync> =
    Arc::new(move |task_id, idx| { broker.mark_dirty_with_delta(task_id, idx); });
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

### 改动 4：ProgressBroker 加 delta 收集

文件：`crates/tachyon-app/src/projection/progress_broker.rs`

结构体（31-41）加字段：
```rust
pending_deltas: Arc<DashMap<String, Vec<u32>>>,
```

`start`（49）、`new_no_aggregator`（107）初始化。

新增方法：
```rust
pub fn mark_dirty_with_delta(&self, task_id: &str, delta_idx: Option<u32>) {
    if let Some(idx) = delta_idx {
        self.pending_deltas.entry(task_id.to_string()).or_default().push(idx);
    }
    self.dirty_tasks.insert(task_id.to_string());
    self.notify.notify_one();
}
```

### 改动 5：build_progress_event + build_initial_progress_event 签名改

文件：`crates/tachyon-app/src/projection/progress_broker.rs:148` 和 `crates/tachyon-app/src/commands/progress_commands.rs:89`

两个函数都需读 `task.fragments_total` + `task.active_concurrency`，`build_progress_event` 额外 take `pending_deltas`。

```rust
// progress_broker.rs:148
fn build_progress_event(
    task_repository: &TaskRepository,
    pending_deltas: &DashMap<String, Vec<u32>>,
) -> ProgressEvent {
    task_repository.iter().map(|r| {
        let id = r.key();
        let t = r.value();
        let completed_delta = pending_deltas
            .get_mut(id)
            .map(|mut d| std::mem::take(&mut *d))
            .unwrap_or_default();
        (id.clone(), TaskProgress {
            id: id.clone(),
            progress: t.progress,
            speed: t.speed,
            downloaded: t.downloaded,
            status: t.status,
            fragments_done: t.fragments_done,
            fragments_total: t.fragments_total,
            active_concurrency: t.active_concurrency,
            completed_delta,
        })
    }).collect()
}
```

`build_initial_progress_event`（progress_commands.rs:89）同构，但 `completed_delta` 首次为空 Vec（不 take pending_deltas，因为首次广播时无 delta）。

调用点：`spawn_aggregator`（progress_broker.rs:95）和 `broadcast_all`（130）加 `&self.pending_deltas`。

### 改动 6：get_task_fragments command（新增）

文件：`crates/tachyon-app/src/commands/fragment_commands.rs`（新文件）

```rust
#[tauri::command]
pub async fn get_task_fragments(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<TaskFragmentsView, AppError> {
    let Some(state) = state.fragment_state_store.get(&task_id) else {
        return Ok(TaskFragmentsView { total: 0, done_indices: vec![] });
    };
    Ok(TaskFragmentsView {
        total: state.total,
        done_indices: state.done_set.iter().copied().collect(),
    })
}
```

注册到 `lib.rs:130-166` 的 `invoke_handler`。

### 改动 7：AppState 加字段 + cleanup 清理

`commands/mod.rs:314` 加 `pub fragment_state_store: FragmentStateStore`。`try_new`/`default` 初始化。

`cleanup_runtime`（download_session.rs:303 调用）加 `state.fragment_state_store.remove(&task_id)`。

### 改动 8：DownloadProgress 加字段

`commands/mod.rs:210` 的 `DownloadProgress` 加 `active_concurrency: u32`。`get_download_progress_inner`（progress_commands.rs:76-86）同步读取。前端 `api.getDownloadProgress(taskId)` 用于单任务进度查询（非 DetailPanel 主路径，但保持字段一致性避免前端类型缺字段）。

## 前端改动

### 改动 1：新增分片 store

文件：`frontend/src/stores/taskFragments.ts`（新文件）

```ts
import { createSignal } from "solid-js";
import { api } from "../api/invoke";

interface TaskFragmentData {
  total: number
  concurrency: number
  doneSet: Set<number>
}

const [fragmentMap, setFragmentMap] = createSignal<Map<string, TaskFragmentData>>(new Map());
let currentLoadToken = 0;

export async function loadTaskFragments(taskId: string) {
  const token = ++currentLoadToken;
  const view = await api.getTaskFragments(taskId);
  if (token !== currentLoadToken) return;  // 竞态防护：已被后续切换覆盖
  const doneSet = new Set<number>(view.doneIndices);
  setFragmentMap(prev => {
    const next = new Map(prev);
    next.set(taskId, { total: view.total, concurrency: 0, doneSet });
    return next;
  });
}

export function clearTaskFragments(taskId: string) {
  setFragmentMap(prev => { const next = new Map(prev); next.delete(taskId); return next; });
}

export function mergeFragmentDelta(taskId: string, delta: number[], concurrency: number) {
  setFragmentMap(prev => {
    const data = prev.get(taskId);
    if (!data) return prev;  // DetailPanel 未打开，忽略（后续首拉会拿完整 doneSet）
    const next = new Map(prev);
    const newSet = new Set(data.doneSet);
    for (const idx of delta) newSet.add(idx);
    next.set(taskId, { ...data, doneSet: newSet, concurrency: concurrency || data.concurrency });
    return next;
  });
}

export function getTaskFragmentData(taskId: string) {
  return fragmentMap().get(taskId);
}
```

### 改动 2：updateProgress 扩展

文件：`frontend/src/stores/downloads.ts:192-289`

在现有 `batch()` 内，每个任务处理时：

```ts
const newFragmentsTotal = p.fragmentsTotal ?? task.fragmentsTotal;
const newConcurrency = p.activeConcurrency ?? 0;

if (hasChanged) {
  setTasksRaw(idx, {
    downloaded: newDownloaded,
    speed: newSpeed,
    status: newStatus,
    progress: newProgress,
    fragmentsDone: newFragmentsDone,
    fragmentsTotal: newFragmentsTotal,  // 新增：覆盖旧估算值
  });
}

// 合并分片 delta 到 fragment store
if (p.completedDelta && p.completedDelta.length > 0) {
  mergeFragmentDelta(id, p.completedDelta, newConcurrency);
} else if (newConcurrency > 0) {
  mergeFragmentDelta(id, [], newConcurrency);
}

// fragmentsTotal 从 0 变非 0：PlanComplete 到达，DetailPanel 若已打开需重拉
if (task.fragmentsTotal === 0 && newFragmentsTotal > 0 && getTaskFragmentData(id) === undefined) {
  loadTaskFragments(id);
}
```

### 改动 3：ChunkMatrix 改数据源（DOM + Canvas 双模式）

文件：`frontend/src/components/ChunkMatrix.tsx`

Props 加 `taskId`：

```ts
interface ChunkMatrixProps {
  taskId: string;            // 新增
  fragmentsTotal: number;
  fragmentsDone: number;
  progress: number;
}
```

DOM 模式（chunks memo，249-271）改为读真实 doneSet：

```ts
const fragData = createMemo(() => getTaskFragmentData(props.taskId));

const chunks = createMemo(() => {
  if (props.fragmentsTotal > AGGREGATE_THRESHOLD) return [];
  const data = fragData();
  // 无 store 数据时回退到旧推算（store 未加载完成期间）
  const doneSet = data?.doneSet ?? new Set<number>();
  const concurrency = data?.concurrency || Math.max(2, Math.round(props.fragmentsTotal / 8));
  const done = props.fragmentsDone;
  const maxDoneIdx = doneSet.size > 0 ? Math.max(...doneSet) : -1;
  const remaining = props.fragmentsTotal - done;
  const band = Math.min(concurrency, Math.max(1, remaining));
  return Array.from({ length: props.fragmentsTotal }, (_, i) => {
    const isDone = doneSet.has(i);
    // 已知折中：maxDoneIdx 之前未完成的分片（如重试中）显示为 pending
    const isDownloading = !isDone && i > maxDoneIdx && i <= maxDoneIdx + band && props.progress < 1;
    const status: ChunkBlock["status"] = isDone ? "done" : isDownloading ? "downloading" : "pending";
    return { index: i, isDone, isDownloading, color: STATUS_COLOR_VARS[status] };
  });
});
```

Canvas 模式（buildBlocks，60-103）改为接收 doneSet：

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
  const band = Math.min(concurrency, Math.max(1, remaining));
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
    // ... status 判定逻辑不变 ...
  }
  return blocks;
}
```

调用处（273-275）改为：
```ts
const blocks = createMemo(() => {
  const data = fragData();
  const doneSet = data?.doneSet ?? new Set<number>();
  const concurrency = data?.concurrency || Math.max(2, Math.round(props.fragmentsTotal / 8));
  return buildBlocks(props.fragmentsTotal, props.fragmentsDone, props.progress, doneSet, concurrency);
});
```

整块下载兜底（新增 effect）：

```ts
createEffect(() => {
  if (props.progress >= 1 && fragData() && fragData()!.doneSet.size === 0) {
    mergeFragmentDelta(props.taskId, [0], 0);
  }
});
```

### 改动 4：DetailPanel 生命周期

文件：`frontend/src/components/DetailPanel.tsx`

Props 传 taskId 给 ChunkMatrix（903-917 区域）：

```tsx
<ChunkMatrix
  taskId={task()!.id}
  fragmentsTotal={task()!.fragmentsTotal}
  fragmentsDone={task()!.fragmentsDone}
  progress={task()!.progress}
/>
```

task 切换时按需加载（在现有 createEffect 104-131 内或新增 effect）：

```ts
createEffect(() => {
  const task = props.task;
  if (!task) return;
  if (getTaskFragmentData(task.id)) return;  // 已有数据，不重复拉
  loadTaskFragments(task.id);
});

onCleanup(() => {
  const task = props.task;
  if (task) clearTaskFragments(task.id);
});
```

### 改动 5：API 新增

文件：`frontend/src/api/invoke.ts`

```ts
getTaskFragments: (taskId: string) =>
  invoke<TaskFragmentsView>('get_task_fragments', { taskId }),
```

## 测试策略

### 后端测试

1. **FragmentProgress 枚举序列化**：PlanComplete 和 Chunk 两变体的 serde round-trip。
2. **plan() 发送 PlanComplete**：构造 DownloadTask，probe + plan，断言 progress_tx 收到 PlanComplete，total 与 `self.fragments.len()` 一致，completed_indices 与续传 done 集一致。
3. **chunk reader match 两分支**：mock progress_rx 发 PlanComplete + Chunk，断言 TaskInfo.fragments_total 被覆盖、FragmentStateStore 初始化、done_set 增量更新。
4. **ProgressBroker delta 收集**：mark_dirty_with_delta 推入 pending_deltas，build_progress_event take 后清空。
5. **get_task_fragments command**：FragmentStateStore 有/无数据两种情况。

### 前端测试

1. **mergeFragmentDelta**：delta 数组合并到 doneSet，concurrency 更新。
2. **loadTaskFragments 竞态防护**：快速切换两次，第二次覆盖第一次。
3. **ChunkMatrix DOM 模式**：doneSet 真实数据渲染，downloading 带宽 = min(concurrency, remaining)。
4. **ChunkMatrix Canvas 模式**：buildBlocks 接收 doneSet，区间 done 比例正确。
5. **整块下载兜底**：progress=1 且 doneSet 空时标 [0] done。

## 已知限制

1. **downloading 推算是近似的**：用 `min(concurrency, remaining)` 从 `maxDoneIdx` 后推。正常顺序下载时准确；乱序完成或分片重试场景下，`maxDoneIdx` 之前的未完成分片（如正在重试的分片）会显示为 pending，而非 downloading。这是 2B' 方案的已知折中——精确并发位置需 StateChange（2F 方案），代价是 FragmentState 移 core + 通道注入，未采纳。

2. **active_concurrency 是静态初始值**：plan() 时的 `recommendation.concurrency`，运行中调度器可能调整 effective_concurrency（`downloader.rs:1025-1049`），但不会回传前端。前端推算的 downloading 带宽可能略大于实际并发度。

3. **续传恢复时 total_downloaded 从 0 开始**：chunk reader 的 `total_downloaded`（`chunk_reader_pool.rs:181`）不读续传快照的已下载字节，从 0 爬升。这是现有代码的行为（非本次改动引入），`fragments_done` 会被 PlanComplete 的 `completed_indices` 正确初始化，但 `downloaded` 字节进度会跳变。Out of scope。

4. **DetailPanel 首次打开时 PlanComplete 已过去的场景**：若任务已进入 downloading 阶段（PlanComplete 已发且 FragmentStateStore 已初始化），DetailPanel 打开时 get_task_fragments 首拉拿到当前 doneSet——正确。若任务还在 probe 阶段（PlanComplete 未发），get_task_fragments 返回 `{total:0, done_indices:[]}`，ChunkMatrix 不渲染（`fragmentsTotal > 0` 的 Show 条件不满足）。任务进入 downloading 后 PlanComplete 到达，`progress-update` 携带的 `fragmentsTotal` 从 0 变非 0——前端需在此变化时触发 `loadTaskFragments` 重拉（见前端改动 2 的 fragmentsTotal 变化检测）。

## 备选方案

### 子问题 1 备选

| 方案 | 机制 | 否决原因 |
|---|---|---|
| 1B. plan 提到 app 层 | 改 run_inner() 结构，app 层显式调 plan() 拿返回值 | 波及所有 test/bench/fuzz 的 execute() 调用契约 |
| 1C. 从 TaskStore 快照读 | 读 completed_fragments | 快照只有 done 索引无元数据；有 checkpoint 延迟（每5片/20事件落盘） |
| 1D. TaskRunner getter + DownloadTask 长存化 | 新增 trait 方法 + Supervisor 持有 DownloadTask | 改 Supervisor 生命周期管理 + TaskRunner trait 契约 |

### 子问题 2 备选

| 方案 | 机制 | 否决原因 |
|---|---|---|
| 2A. 纯轮询 | get_task_fragments 每 2s 轮询 | 2s 延迟导致 DetailPanel 打开时视觉跳变 |
| 2B. 纯增量 done 数组 | ProgressPayload 带 completedDelta，无 concurrency | downloading 推算用 total-done，大文件下 downloading 区域过大 |
| 2C. 全量 done 数组 | progress-update 每次带完整 doneSet | 大文件 done 数组可达数千，250ms 重复传 |
| 2D/2F. StateChange 逐片状态 | worker spawn 内发 StateChange{Downloading} + FragmentState 移 core | 通道丢失风险需 5s 校正轮询；FragmentState 跨 crate 移动；downloading 精度收益不抵代价 |
| 2E. 纯轮询无事件 | 不用 progress-update 传分片数据 | 与现有事件流割裂，两套数据可能不一致 |

## 改动文件清单

### 后端

| 文件 | 改动 |
|---|---|
| `crates/tachyon-core/src/types.rs:359` | struct→enum，失 Copy+Eq |
| `crates/tachyon-engine/src/downloader.rs:1634` | report_progress 构造体改 Chunk |
| `crates/tachyon-engine/src/downloader.rs:1860` | 完成 send 构造体改 Chunk |
| `crates/tachyon-engine/src/downloader.rs:852` | 新增 PlanComplete send |
| `crates/tachyon-app/src/runtime/chunk_reader_pool.rs:28,161` | ChunkReaderJob 加字段 + match 两分支 |
| `crates/tachyon-app/src/runtime/download_session.rs:270-281` | 构造 on_progress 改签名 + job 加 store |
| `crates/tachyon-app/src/projection/progress_broker.rs:31,49,107,148` | 加 pending_deltas + mark_dirty_with_delta + build_progress_event 签名 |
| `crates/tachyon-app/src/projection/fragment_state_store.rs` | 新文件：TaskFragmentState + FragmentStateStore |
| `crates/tachyon-app/src/commands/fragment_commands.rs` | 新文件：get_task_fragments command |
| `crates/tachyon-app/src/commands/progress_commands.rs:89` | build_initial_progress_event 加字段 |
| `crates/tachyon-app/src/commands/mod.rs:108,210,225,314` | TaskInfo/DownloadProgress/TaskProgress 加字段 + AppState 加 store |
| `crates/tachyon-app/src/lib.rs:130` | 注册 get_task_fragments |

### 前端

| 文件 | 改动 |
|---|---|
| `frontend/src/types.ts:183` | ProgressPayload 加字段 + TaskFragmentsView 新增 |
| `frontend/src/stores/taskFragments.ts` | 新文件：分片 store |
| `frontend/src/stores/downloads.ts:192` | updateProgress 合并 delta + fragmentsTotal |
| `frontend/src/components/ChunkMatrix.tsx` | DOM + Canvas 双模式改读 doneSet + buildBlocks 签名改 |
| `frontend/src/components/DetailPanel.tsx` | 生命周期加载/清理 + props 加 taskId |
| `frontend/src/api/invoke.ts` | getTaskFragments |
