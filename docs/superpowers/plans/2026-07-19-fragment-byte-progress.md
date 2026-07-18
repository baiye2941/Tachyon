# 分片字节级半填充弹簧进度 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让前端 ChunkMatrix 在下载过程中实时显示每片字节级半填充进度(弹簧物理驱动),根治「分片只在下载后显示」BUG——根因是后端 `frag_bytes` 数据从未进入 Tauri 事件管道,前端 tooltip 把 downloading 状态写死成 50%。

**Architecture:** 后端把 `chunk_reader_pool.rs:251` 已有的 `frag_bytes: HashMap<u32,u64>` 通过扩 `ProgressCallback` 签名透传给 `ProgressBroker` 的新 `pending_fragment_bytes: DashMap<String, Vec<FragmentByteProgress>>`;`build_progress_event` 取出填入 `TaskProgress.fragment_bytes`(快照式,仅含活跃分片,数量 = 并发数 N ≤ 16,非 100)。前端 `taskFragments.ts` 扩 `bytesMap: Map<number, number>`,ChunkMatrix DOM 格子新增 `<FragmentFill>` 子组件(motionone spring 驱动 `transform: scaleX`),Canvas 块用渐变填充深度表示块进度;修 `ChunkMatrix.tsx:618` 写死的 `percent = 50`。顺带做 3 个外壳微调(侧边栏轨道化选中速度线、状态栏峰值标记、任务行微型光迹)。复用现有 250ms aggregator,不新增高频 Tauri 事件。带宽:16 分片 × 12 字节 × 4Hz ≈ 0.75KB/s,可忽略。

**Tech Stack:** Rust + Tauri v2(后端),SolidJS + TypeScript + @motionone/solid + @tanstack/solid-virtual(前端)

## Global Constraints

- cargo clippy MUST 零警告(`-D warnings`),`cargo nextest run --all` 全通过
- 测试覆盖率 MUST >= 90%(协议层/网络 IO/Tauri 命令等依赖外部服务的模块排除计算)
- 所有 unsafe 代码 MUST 有 Safety 注释,禁止引入未标注的 unsafe
- 注释/文档/提交信息使用中文,代码标识符使用英文,不使用 emoji
- 前端 MUST 使用 Bun + Tauri v2,SolidJS(非 React),动效用 `@motionone/solid`(非 `motion/react`)
- 提交格式:`<类型>(<范围>): <简要描述>`(中文)
- Motion MUST 仅动 `transform`/`opacity`,MUST honor `prefers-reduced-motion`(用现有 `useReducedMotion` hook)
- 配色不重做:复用现有 token(主强调 `--color-accent-primary` #35dde2、完成 `--color-status-completed`、下载中 `--color-status-downloading`),不引入新主色

---

## File Structure

### 后端修改文件
- `crates/tachyon-app/src/commands/mod.rs` — `TaskProgress` 加 `fragment_bytes: Vec<FragmentByteProgress>` 字段 + 新增 `FragmentByteProgress` struct
- `crates/tachyon-app/src/runtime/chunk_reader_pool.rs` — `ProgressCallback` 签名加 `bytes: Option<&HashMap<u32, u64>>` 参数 + `run_chunk_reader` 每事件调用时传 `&frag_bytes`
- `crates/tachyon-app/src/runtime/download_session.rs` — `on_progress` 闭包签名对齐新参数,透传给 broker
- `crates/tachyon-app/src/projection/progress_broker.rs` — 新增 `pending_fragment_bytes: DashMap<String, Vec<FragmentByteProgress>>` + `mark_dirty_with_delta` 改名为 `mark_dirty` 并新签名接收字节 + `build_progress_event` 取出填入字段
- `crates/tachyon-app/src/commands/progress_commands.rs` — `build_initial_progress_event` 与所有 `TaskProgress` 字面量构造补 `fragment_bytes: vec![]` 字段

### 后端测试文件(修改现有 `#[cfg(test)]` mod)
- `crates/tachyon-app/src/projection/progress_broker.rs` tests — 字面量补字段 + 新增字节透传测试
- `crates/tachyon-app/src/commands/progress_commands.rs` tests — 字面量补字段
- `crates/tachyon-app/src/runtime/chunk_reader_pool.rs` tests — 新增 callback 接收字节的测试

### 前端修改文件
- `frontend/src/types.ts` — `ProgressPayload` 加 `fragmentBytes?: FragmentByteProgress[]` + 新增 `FragmentByteProgress` interface
- `frontend/src/stores/taskFragments.ts` — `TaskFragmentData` 加 `bytesMap: Map<number, number>` + `mergeFragmentDelta` 合并字节
- `frontend/src/stores/downloads.ts` — `updateProgress` 把 `fragmentBytes` 传给 `mergeFragmentDelta`
- `frontend/src/components/ChunkMatrix.tsx` — 新增 `<FragmentFill>` 子组件(DOM 格子半填充弹簧) + Canvas 块渐变深度 + 修 line 618 写死 `percent = 50`
- `frontend/src/styles/components/chunk-matrix.css` — 加 `.chunk-cell-fill` 充能条样式
- `frontend/src/components/ChunkMatrix.tsx` 的 tooltip — downloading 状态显示真实 `bytesMap` 百分比
- `frontend/src/components/Sidebar.tsx` + `frontend/src/styles/components/sidebar.css` — 选中态速度线流动
- `frontend/src/components/StatusBar.tsx` + `frontend/src/styles/components/status-bar.css` — 速度峰值标记
- `frontend/src/components/TaskItem.tsx` + `frontend/src/styles/components/task-item.css` — 下载中微型光迹

### 前端测试文件(新建)
- `frontend/src/components/__tests__/FragmentFill.spec.tsx` — 弹簧进度条渲染与 reduced-motion 降级
- `frontend/src/stores/__tests__/taskFragments-bytes.spec.ts` — bytesMap 合并逻辑

---

## 设计决定(已定,写进 plan 供 reviewer 核验)

1. **快照式而非 delta 式**:每 250ms tick 发送当前所有**活跃分片**(在 `downloading_set` 里的)的字节数快照。已完成分片不在 `fragment_bytes` 里(它们在 `doneSet`),未开始分片进度为 0 也不在里头。所以 `fragment_bytes` 只含活跃分片,数量 = 并发数 N(通常 ≤ 8-16),不是 100。前端无状态、幂等、丢包自愈。
2. **`compute_progress_delta` 用 `PartialEq` 比较**——新增字段后,任一活跃分片字节变化即触发 delta 推送,这正是下载中持续推送所需。
3. **`ProgressCallback` 签名扩展**:从 `Fn(&str, Option<ProgressDelta>)` 扩为 `Fn(&str, Option<ProgressDelta>, &[FragmentByteEntry])`,其中 `FragmentByteEntry { index: u32, downloaded: u64 }`(借用切片,零分配)。每事件调用时传 `&frag_bytes.iter().map(|(&k,&v)| FragmentByteEntry{k,v}).collect::<Vec<_>>()`——这个小 Vec 分配每事件一次,开销可忽略(chunk 写入本就是 IO 密集,这点 Vec 在噪声内)。
4. **不新增 Tauri 事件**:复用 `progress-update`,字段加在 `TaskProgress` payload 里。

---

## Task 1: 后端 `TaskProgress` 加 `fragment_bytes` 字段 + `FragmentByteProgress` struct

**Files:**
- Modify: `crates/tachyon-app/src/commands/mod.rs:251-277`(TaskProgress struct)
- Test: `crates/tachyon-app/src/commands/mod.rs` 内 `#[cfg(test)]` mod(若存在则补,否则本 task 仅改 struct,字段默认值 `vec![]` 保证向后兼容)

**Interfaces:**
- Produces: `TaskProgress.fragment_bytes: Vec<FragmentByteProgress>`(serde `#[serde(default, skip_serializing_if = "Vec::is_empty")]`),`FragmentByteProgress { index: u32, downloaded: u64 }`(`#[serde(rename_all = "camelCase")]`)

- [ ] **Step 1: 写失败测试 — FragmentByteProgress 序列化为 camelCase**

在 `crates/tachyon-app/src/commands/mod.rs` 末尾(或已有 `#[cfg(test)]` mod 内)加测试。先确认文件末尾结构:

Run: `grep -n "cfg(test)" crates/tachyon-app/src/commands/mod.rs | head -5`
Expected: 显示行号(若无线,则在文件末尾新建 `#[cfg(test)]` mod)

加测试(若已有 mod tests 则在其内,否则新建):

```rust
#[cfg(test)]
mod fragment_bytes_tests {
    use super::*;

    #[test]
    fn fragment_byte_progress_serializes_camel_case() {
        let entry = FragmentByteProgress {
            index: 3,
            downloaded: 524288,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"index\""));
        assert!(json.contains("\"downloaded\""));
        assert!(!json.contains("fragment_index"), "应是 camelCase index 而非 fragment_index");
    }

    #[test]
    fn task_progress_fragment_bytes_default_empty() {
        let tp = TaskProgress {
            id: "t1".to_string(),
            progress: 0.0,
            speed: 0,
            downloaded: 0,
            status: DownloadState::Pending,
            fragments_done: 0,
            fragments_total: 0,
            active_concurrency: 0,
            file_size: None,
            completed_delta: vec![],
            started_delta: vec![],
            error_reason: None,
            fragment_bytes: vec![],
        };
        let json = serde_json::to_string(&tp).unwrap();
        // skip_serializing_if = Vec::is_empty,空时不应出现在 JSON
        assert!(!json.contains("fragmentBytes"), "空 fragment_bytes 应被 skip");
    }

    #[test]
    fn task_progress_fragment_bytes_serialized_when_non_empty() {
        let tp = TaskProgress {
            id: "t1".to_string(),
            progress: 0.5,
            speed: 100,
            downloaded: 512,
            status: DownloadState::Downloading,
            fragments_done: 1,
            fragments_total: 4,
            active_concurrency: 2,
            file_size: Some(1024),
            completed_delta: vec![],
            started_delta: vec![],
            error_reason: None,
            fragment_bytes: vec![FragmentByteProgress {
                index: 1,
                downloaded: 256,
            }],
        };
        let json = serde_json::to_string(&tp).unwrap();
        assert!(json.contains("fragmentBytes"), "非空 fragment_bytes 应序列化");
        assert!(json.contains("\"index\":1"));
        assert!(json.contains("\"downloaded\":256"));
    }
}
```

- [ ] **Step 2: 运行测试,确认失败**

Run: `cargo nextest run -p tachyon-app fragment_byte_progress_serializes_camel_case 2>&1 | tail -15`
Expected: 编译错误 `cannot find type `FragmentByteProgress` in this scope` / `no field fragment_bytes on type TaskProgress`

- [ ] **Step 3: 实现 — 在 TaskProgress 上方加 FragmentByteProgress + TaskProgress 加字段**

在 `crates/tachyon-app/src/commands/mod.rs` 的 `TaskProgress` struct 定义**之前**(line 251 前)插入:

```rust
/// 单个活跃分片的字节级进度快照(仅含 downloading_set 中的分片)
///
/// 每 250ms aggregator tick 随 progress-update 发送。已完成分片不在其中
/// (它们进 completed_delta / doneSet),未开始分片进度为 0 也不在其中。
/// 数量 = 当前活跃并发数 N(通常 ≤ 16),非分片总数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FragmentByteProgress {
    pub index: u32,
    pub downloaded: u64,
}
```

在 `TaskProgress` struct 的 `error_reason` 字段**之后**(line 276 后,struct 闭合 `}` 之前)加:

```rust
    /// 活跃分片字节级进度快照(仅 downloading_set 中的分片)。
    /// 快照式:前端无状态、幂等、丢包自愈。空时 skip 以省带宽。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fragment_bytes: Vec<FragmentByteProgress>,
```

- [ ] **Step 4: 运行测试,确认通过**

Run: `cargo nextest run -p tachyon-app fragment_byte_progress 2>&1 | tail -15`
Expected: 3 个测试 PASS

- [ ] **Step 5: 确认全 crate 编译(此时旧字面量会报错,记录待 Task 3 修)**

Run: `cargo build -p tachyon-app 2>&1 | grep -c "missing field \`fragment_bytes\`"`
Expected: 输出一个数字 ≥ 1(表示有若干处字面量需补字段,Task 3 统一处理)

- [ ] **Step 6: 提交**

```bash
git add crates/tachyon-app/src/commands/mod.rs
git commit -m "feat(app): TaskProgress 加 fragment_bytes 字段与 FragmentByteProgress 类型"
```

---

## Task 2: 后端 ProgressBroker 透传字节 + build_progress_event 填充

**Files:**
- Modify: `crates/tachyon-app/src/projection/progress_broker.rs`(struct ProgressBroker 加字段 + mark_dirty_with_delta 改签名 + build_progress_event 取出)
- Test: `crates/tachyon-app/src/projection/progress_broker.rs` `#[cfg(test)]` mod

**Interfaces:**
- Consumes: Task 1 的 `FragmentByteProgress`、`TaskProgress.fragment_bytes`
- Produces: `ProgressBroker::mark_dirty_with_delta(task_id, delta, fragment_bytes)` 新签名;`build_progress_event` 填充 `fragment_bytes`

- [ ] **Step 1: 写失败测试 — build_progress_event 填充 fragment_bytes**

在 `crates/tachyon-app/src/projection/progress_broker.rs` 的 `#[cfg(test)] mod tests` 内(line 365 附近)加测试。注意:现有 `test_build_progress_event_with_tasks` 等用 `TaskProgress { ... }` 字面量构造,**本步骤先不改它们**(它们会因缺 `fragment_bytes` 编译失败,Step 3 的签名改动会触发,Task 3 统一补)。先加**新**测试:

```rust
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

        // 携带 2 个活跃分片字节进度
        broker.mark_dirty_with_delta(
            "t-bytes",
            None,
            vec![
                crate::commands::FragmentByteProgress { index: 0, downloaded: 256 },
                crate::commands::FragmentByteProgress { index: 1, downloaded: 128 },
            ],
        );
        broker.broadcast_all();

        let event = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("应收到 broadcast")
            .expect("broadcast 不应关闭");
        let tp = event.get("t-bytes").expect("t-bytes 应在事件中");
        assert_eq!(tp.fragment_bytes.len(), 2, "fragment_bytes 应含 2 个活跃分片");
        // 按 index 排序后断言(HashMap 迭代序不确定)
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
        repository.insert(
            "t-term".to_string(),
            TaskInfo {
                id: "t-term".to_string(),
                url: "https://example.com/a.bin".to_string(),
                file_name: "a.bin".to_string(),
                file_size: Some(1024),
                downloaded: 1024,
                speed: 0,
                status: DownloadState::Completed,
                progress: 1.0,
                fragments_total: 4,
                fragments_done: 4,
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
        let broker = ProgressBroker::new_no_aggregator(repository.clone());
        let mut rx = broker.subscribe();

        // 先推字节,再清空(传空 Vec)
        broker.mark_dirty_with_delta(
            "t-term",
            None,
            vec![crate::commands::FragmentByteProgress { index: 0, downloaded: 256 }],
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
```

- [ ] **Step 2: 运行测试,确认失败**

Run: `cargo nextest run -p tachyon-app test_fragment_bytes 2>&1 | tail -15`
Expected: 编译错误 `no method named mark_dirty_with_delta takes 3 arguments` / `no field fragment_bytes on TaskProgress 字面量`(后者因 Task 1 已加字段,应是签名错误)

- [ ] **Step 3: 实现 — ProgressBroker 加 pending_fragment_bytes + 改 mark_dirty_with_delta 签名 + build_progress_event 取出**

**3a.** 在 `ProgressBroker` struct(line 73-89)加字段。在 `pending_started` 字段后加:

```rust
    /// 每任务本周期活跃分片字节进度快照(仅 downloading_set 中的分片)
    pub(crate) pending_fragment_bytes: Arc<DashMap<String, Vec<crate::commands::FragmentByteProgress>>>,
```

**3b.** 在两个构造 `start`(line 97-109)与 `new_no_aggregator`(line 169-181)的 Self 字段列表里,`pending_started` 行后加:

```rust
            pending_fragment_bytes: Arc::new(DashMap::new()),
```

**3c.** 在 `spawn_aggregator`(line 116-164)内,`let pending_started = self.pending_started.clone();` 后加:

```rust
        let pending_fragment_bytes = self.pending_fragment_bytes.clone();
```

并在 `loop` 内 `build_progress_event` 调用处(line 149-153)改为传三参:

```rust
                let event = build_progress_event(
                    &task_repository_ref,
                    &pending_completed,
                    &pending_started,
                    &pending_fragment_bytes,
                );
```

**3d.** 在 `broadcast_all`(line 237-251)内同样改 `build_progress_event` 调用为传 `&self.pending_fragment_bytes`:

```rust
        let event = build_progress_event(
            &self.task_repository,
            &self.pending_completed,
            &self.pending_started,
            &self.pending_fragment_bytes,
        );
```

**3e.** 改 `mark_dirty_with_delta` 签名(line 209-231),加 `fragment_bytes: Vec<FragmentByteProgress>` 参数,存入 `pending_fragment_bytes`(覆盖式快照,非追加):

```rust
    /// 标记任务进度变化,记录分片状态变更增量(started/completed)+ 活跃分片字节快照
    ///
    /// 竞态消除:当 Completed(idx) 到达时,从 pending_started 中移除 idx(若存在),
    /// 避免同一分片的 Started 增量在跨窗口场景下被推送给前端导致"幽灵 downloading"。
    ///
    /// fragment_bytes 为快照式覆盖:每次调用覆盖该任务本周期的字节快照。
    /// 空 Vec 表示无活跃分片(终态或全部完成)。
    pub fn mark_dirty_with_delta(
        &self,
        task_id: &str,
        delta: Option<ProgressDelta>,
        fragment_bytes: Vec<crate::commands::FragmentByteProgress>,
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
                    // 完成的分片不再出现在字节快照中(过滤掉)
                    if let Some(mut bytes) = self.pending_fragment_bytes.get_mut(task_id) {
                        bytes.retain(|e| e.index != idx);
                    }
                    self.pending_completed
                        .entry(task_id.to_string())
                        .or_default()
                        .push(idx);
                }
            }
        }
        // 字节快照覆盖式写入(空 Vec 也写入,表示清空)
        self.pending_fragment_bytes
            .insert(task_id.to_string(), fragment_bytes);
        self.notify.notify_one();
    }
```

**3f.** 改 `build_progress_event` 签名与实现(line 267-304):

```rust
fn build_progress_event(
    task_repository: &TaskRepository,
    pending_completed: &DashMap<String, Vec<u32>>,
    pending_started: &DashMap<String, Vec<u32>>,
    pending_fragment_bytes: &DashMap<String, Vec<crate::commands::FragmentByteProgress>>,
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
```

- [ ] **Step 4: 运行新测试,确认通过**

Run: `cargo nextest run -p tachyon-app test_fragment_bytes 2>&1 | tail -15`
Expected: 2 个新测试 PASS

- [ ] **Step 5: 提交**

```bash
git add crates/tachyon-app/src/projection/progress_broker.rs
git commit -m "feat(broker): ProgressBroker 透传活跃分片字节快照到 progress-update"
```

---

## Task 3: 后端补全所有 TaskProgress 字面量构造 + progress_commands 适配

**Files:**
- Modify: `crates/tachyon-app/src/commands/progress_commands.rs`(`build_initial_progress_event` + 所有测试字面量)
- Modify: `crates/tachyon-app/src/projection/progress_broker.rs` tests mod(现有字面量)
- Modify: `crates/tachyon-app/src/runtime/chunk_reader_pool.rs`(`on_progress` 调用处暂不改,Task 4 处理)

**Interfaces:**
- Consumes: Task 1 的 `fragment_bytes` 字段、Task 2 的 `mark_dirty_with_delta` 三参签名
- Produces: 全 crate 编译通过、`cargo nextest run -p tachyon-app` 全绿

- [ ] **Step 1: 写失败测试 — build_initial_progress_event 产生 fragment_bytes 空字段**

在 `crates/tachyon-app/src/commands/progress_commands.rs` 的 `#[cfg(test)] mod tests` 内加测试:

```rust
    #[test]
    fn test_build_initial_progress_event_has_empty_fragment_bytes() {
        let state = test_state();
        let _id = create_task_inner(
            &state,
            "https://example.com/init.bin".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        // build_initial_progress_event 是同步 fn,但 create_task_inner 是 async,需在 tokio test 里调
        let event = build_initial_progress_event(&state.domain.task_repository);
        let tp = event.values().next().expect("应至少一个任务");
        assert!(tp.fragment_bytes.is_empty(), "初始快照 fragment_bytes 应为空");
    }
```

注意:`build_initial_progress_event` 当前是同步 fn 但测试用 `create_task_inner`(async)。需把测试标 `#[tokio::test]`。修正:

```rust
    #[tokio::test]
    async fn test_build_initial_progress_event_has_empty_fragment_bytes() {
        let state = test_state();
        let _id = create_task_inner(
            &state,
            "https://example.com/init.bin".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        let event = build_initial_progress_event(&state.domain.task_repository);
        let tp = event.values().next().expect("应至少一个任务");
        assert!(tp.fragment_bytes.is_empty());
    }
```

- [ ] **Step 2: 运行测试,确认失败(因字面量缺字段编译错误)**

Run: `cargo build -p tachyon-app 2>&1 | grep "missing field" | head -20`
Expected: 列出所有缺 `fragment_bytes` 字段的 `TaskProgress { ... }` 构造点(progress_commands.rs 与 progress_broker.rs tests)

- [ ] **Step 3: 补全 progress_commands.rs 所有字面量**

在 `crates/tachyon-app/src/commands/progress_commands.rs`:
- `build_initial_progress_event`(line 99-126)的 `TaskProgress { ... }` 内,`error_reason: None,` 后加 `fragment_bytes: vec![],`
- 所有 `#[cfg(test)]` mod 内的 `TaskProgress { ... }` 字面量(line 277-411 多处),每个的 `error_reason: ...,` 后加 `fragment_bytes: vec![],`

可用替换:对每个 `error_reason: None,\n            },` 与 `error_reason: None,\n        };` 模式补字段。手动逐个加,或用 sed 批量:

```bash
# 谨慎:仅在本文件,且确认每处确为 TaskProgress 字面量
# 先 dry-run 查看将改的行
grep -n "error_reason: None," crates/tachyon-app/src/commands/progress_commands.rs
```

手动 Edit 每处,在 `error_reason: None,` 后加一行 `fragment_bytes: vec![],`(保持缩进一致)。

- [ ] **Step 4: 补全 progress_broker.rs tests mod 字面量**

在 `crates/tachyon-app/src/projection/progress_broker.rs` 的 `#[cfg(test)] mod tests` 内,所有 `TaskProgress { ... }` 字面量(line 386-432、454-475、512-533、552-573、618-640、828-850 等多处)的 `error_reason: ...,` 后加 `fragment_bytes: vec![],`。

同样手动 Edit 每处。

- [ ] **Step 5: 运行全 crate 测试**

Run: `cargo nextest run -p tachyon-app 2>&1 | tail -20`
Expected: 全部 PASS(含 Task 1/2 新增测试 + 现有测试)

- [ ] **Step 6: clippy 零警告**

Run: `cargo clippy -p tachyon-app --all-targets -- -D warnings 2>&1 | tail -10`
Expected: 无输出(零警告)

- [ ] **Step 7: 提交**

```bash
git add crates/tachyon-app/src/commands/progress_commands.rs crates/tachyon-app/src/projection/progress_broker.rs
git commit -m "fix(app): 补全 TaskProgress 字面量 fragment_bytes 字段并适配三参 mark_dirty"
```

---

## Task 4: 后端 chunk_reader_pool callback 透传 frag_bytes + download_session wiring

**Files:**
- Modify: `crates/tachyon-app/src/runtime/chunk_reader_pool.rs`(`ProgressCallback` 类型别名 + `run_chunk_reader` 调用处)
- Modify: `crates/tachyon-app/src/runtime/download_session.rs:296-299`(on_progress 闭包)
- Test: `crates/tachyon-app/src/runtime/chunk_reader_pool.rs` `#[cfg(test)]` mod

**Interfaces:**
- Consumes: Task 2 的 `mark_dirty_with_delta(task_id, delta, fragment_bytes)` 三参签名
- Produces: `ProgressCallback = Arc<dyn Fn(&str, Option<ProgressDelta>, &[FragmentByteEntry]) + Send + Sync>`;`FragmentByteEntry { index: u32, downloaded: u64 }`(定义在 chunk_reader_pool.rs)

- [ ] **Step 1: 写失败测试 — callback 接收字节切片**

在 `crates/tachyon-app/src/runtime/chunk_reader_pool.rs` 的 `#[cfg(test)] mod tests` 内加测试(用自定义 callback 捕获字节):

```rust
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

        // 捕获 callback 收到的字节切片
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

        // 发送分片 0 的增量字节进度(未完成)
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
            "callback 应收到分片 0 字节 300,实际: {:?}", g
        );
    }
```

- [ ] **Step 2: 运行测试,确认失败**

Run: `cargo nextest run -p tachyon-app test_callback_receives_fragment_bytes 2>&1 | tail -15`
Expected: 编译错误(`ProgressCallback` 当前是 2 参 `Fn(&str, Option<ProgressDelta>)`,测试调 3 参)

- [ ] **Step 3: 实现 — 定义 FragmentByteEntry + 改 ProgressCallback 签名 + run_chunk_reader 传字节**

**3a.** 在 `crates/tachyon-app/src/runtime/chunk_reader_pool.rs` 顶部(在 `ProgressDelta` enum 定义后,line 24 附近)加:

```rust
/// 活跃分片字节进度条目(传给 ProgressCallback 的切片元素)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FragmentByteEntry {
    pub index: u32,
    pub downloaded: u64,
}
```

**3b.** 改 `ProgressCallback` 类型别名(line 27):

```rust
/// 进度变化回调:参数为 (task_id, delta, fragment_bytes),
/// fragment_bytes 为当前所有活跃分片(downloading_set 中)的字节快照切片。
pub type ProgressCallback = Arc<dyn Fn(&str, Option<ProgressDelta>, &[FragmentByteEntry]) + Send + Sync>;
```

**3c.** 在 `run_chunk_reader` 内,每个 `FragmentProgress::Chunk` 事件处理末尾(line 387-396 的 `on_progress` 调用),改为构造字节切片传入。原代码:

```rust
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
```

改为:

```rust
                // 构造活跃分片字节快照(frag_bytes 当前状态),传给 ProgressBroker
                if let Some(ref callback) = on_progress {
                    let bytes_snapshot: Vec<FragmentByteEntry> = frag_bytes
                        .iter()
                        .map(|(&k, &v)| FragmentByteEntry { index: k, downloaded: v })
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
```

注意:`frag_bytes` 在完成事件分支已先 `remove(&fragment_index)`(line 337),所以完成事件的 snapshot 不含该分片——符合「完成分片不出现在字节快照」语义。

**3d.** 同样改 `Started` 分支(line 296-308)的 `on_progress` 调用,传 `&[]`(Started 事件时 frag_bytes 可能尚无该分片条目,传空切片,broker 会在后续 Chunk 事件拿到真实字节)。原:

```rust
                if let Some(ref callback) = on_progress {
                    callback(&task_id, Some(ProgressDelta::Started(fragment_index)));
                }
```

改为:

```rust
                if let Some(ref callback) = on_progress {
                    let bytes_snapshot: Vec<FragmentByteEntry> = frag_bytes
                        .iter()
                        .map(|(&k, &v)| FragmentByteEntry { index: k, downloaded: v })
                        .collect();
                    callback(&task_id, Some(ProgressDelta::Started(fragment_index)), &bytes_snapshot);
                }
```

**3e.** 同样改 `PlanComplete` 分支(line 287-289)的 `callback(&task_id, None)` 调用为三参 `callback(&task_id, None, &[])`。

- [ ] **Step 4: 改 download_session.rs on_progress 闭包**

在 `crates/tachyon-app/src/runtime/download_session.rs:296-299`,把:

```rust
        let on_progress: crate::runtime::chunk_reader_pool::ProgressCallback =
            Arc::new(move |task_id, delta| {
                broker.mark_dirty_with_delta(task_id, delta);
            });
```

改为:

```rust
        let on_progress: crate::runtime::chunk_reader_pool::ProgressCallback =
            Arc::new(move |task_id, delta, bytes| {
                let fb: Vec<crate::commands::FragmentByteProgress> = bytes
                    .iter()
                    .map(|e| crate::commands::FragmentByteProgress {
                        index: e.index,
                        downloaded: e.downloaded,
                    })
                    .collect();
                broker.mark_dirty_with_delta(task_id, delta, fb);
            });
```

- [ ] **Step 5: 运行测试**

Run: `cargo nextest run -p tachyon-app test_callback_receives_fragment_bytes 2>&1 | tail -15`
Expected: PASS

- [ ] **Step 6: 运行全 crate 测试 + clippy**

Run: `cargo nextest run -p tachyon-app 2>&1 | tail -10 && cargo clippy -p tachyon-app --all-targets -- -D warnings 2>&1 | tail -5`
Expected: 全测试 PASS,零 clippy 警告

- [ ] **Step 7: 提交**

```bash
git add crates/tachyon-app/src/runtime/chunk_reader_pool.rs crates/tachyon-app/src/runtime/download_session.rs
git commit -m "feat(runtime): ChunkReaderPool callback 透传活跃分片字节快照切片"
```

---

## Task 5: 后端全量回归(跨 crate 编译 + 测试 + 覆盖率)

**Files:**
- 无修改,仅验证

- [ ] **Step 1: 全 crate 构建**

Run: `cargo build --all 2>&1 | tail -10`
Expected: 编译成功,零错误

- [ ] **Step 2: 全 crate 测试**

Run: `cargo nextest run --all 2>&1 | tail -20`
Expected: 全部 PASS

- [ ] **Step 3: clippy 全 crate 零警告**

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -5`
Expected: 无输出

- [ ] **Step 4: 核心逻辑 crate 覆盖率门禁**

Run: `cargo llvm-cov -p tachyon-app --fail-under-lines 90 --summary-only 2>&1 | tail -15`
Expected: 覆盖率 >= 90%(若 tachyon-app 排除计算则跳过此步,记录于回复)

- [ ] **Step 5: 无提交(纯验证步)**

---

## Task 6: 前端 types.ts + taskFragments store 扩 bytesMap

**Files:**
- Modify: `frontend/src/types.ts`(ProgressPayload 加 fragmentBytes + 新增 FragmentByteProgress interface)
- Modify: `frontend/src/stores/taskFragments.ts`(TaskFragmentData 加 bytesMap + mergeFragmentDelta 合并)
- Modify: `frontend/src/stores/downloads.ts`(updateProgress 传 fragmentBytes)
- Test: `frontend/src/stores/__tests__/taskFragments-bytes.spec.ts`(新建)

**Interfaces:**
- Consumes: 后端 `ProgressPayload.fragmentBytes`(Task 1-4 产出)
- Produces: `TaskFragmentData.bytesMap: Map<number, number>`;`mergeFragmentDelta(taskId, completedDelta, startedDelta, fragmentBytes?)`

- [ ] **Step 1: 写失败测试 — bytesMap 合并逻辑**

新建 `frontend/src/stores/__tests__/taskFragments-bytes.spec.ts`:

```typescript
import { describe, it, expect, beforeEach } from "vitest";
import {
  loadTaskFragments,
  mergeFragmentDelta,
  getTaskFragmentData,
  clearTaskFragments,
} from "../taskFragments";

// mock invoke 避免真实 Tauri 调用
vi.mock("../../api/invoke", () => ({
  api: {
    getTaskFragments: vi.fn().mockResolvedValue({
      total: 4,
      doneIndices: [],
      downloadingIndices: [],
    }),
  },
}));

describe("taskFragments bytesMap", () => {
  beforeEach(() => {
    clearTaskFragments("t-bytes");
  });

  it("mergeFragmentDelta 合并 fragmentBytes 到 bytesMap", async () => {
    await loadTaskFragments("t-bytes");
    mergeFragmentDelta("t-bytes", [], [0, 1], [
      { index: 0, downloaded: 256 },
      { index: 1, downloaded: 128 },
    ]);
    const data = getTaskFragmentData("t-bytes");
    expect(data).toBeDefined();
    expect(data!.bytesMap.get(0)).toBe(256);
    expect(data!.bytesMap.get(1)).toBe(128);
  });

  it("快照覆盖:第二次 merge 覆盖旧字节,不在快照中的分片被移除", async () => {
    await loadTaskFragments("t-bytes");
    mergeFragmentDelta("t-bytes", [], [0, 1], [
      { index: 0, downloaded: 100 },
      { index: 1, downloaded: 200 },
    ]);
    mergeFragmentDelta("t-bytes", [], [1], [
      { index: 1, downloaded: 250 },
    ]);
    const data = getTaskFragmentData("t-bytes");
    expect(data!.bytesMap.has(0)).toBe(false);
    expect(data!.bytesMap.get(1)).toBe(250);
  });

  it("completedDelta 的分片从 bytesMap 移除", async () => {
    await loadTaskFragments("t-bytes");
    mergeFragmentDelta("t-bytes", [], [0], [{ index: 0, downloaded: 100 }]);
    mergeFragmentDelta("t-bytes", [0], [], []);
    const data = getTaskFragmentData("t-bytes");
    expect(data!.bytesMap.has(0)).toBe(false);
    expect(data!.doneSet.has(0)).toBe(true);
  });
});
```

- [ ] **Step 2: 运行测试,确认失败**

Run: `cd frontend && bun run test -- taskFragments-bytes 2>&1 | tail -20`
Expected: FAIL(`fragmentBytes` 参数未定义 / `bytesMap` 属性不存在)

- [ ] **Step 3: 实现 types.ts**

在 `frontend/src/types.ts` 的 `ProgressPayload` interface(line 260-276),`startedDelta` 后加:

```typescript
  /** 活跃分片字节级进度快照(仅 downloading_set 中的分片)。每 250ms tick 覆盖式推送 */
  fragmentBytes?: FragmentByteProgress[]
```

并在 `ProgressPayload` 上方加 interface:

```typescript
/** 单个活跃分片的字节级进度(与后端 FragmentByteProgress 对齐) */
export interface FragmentByteProgress {
  index: number
  downloaded: number
}
```

- [ ] **Step 4: 实现 taskFragments.ts**

在 `frontend/src/stores/taskFragments.ts`:

**4a.** `TaskFragmentData` interface(line 5-11)加字段:

```typescript
export interface TaskFragmentData {
  total: number;
  doneSet: Set<number>;
  downloadingSet: Set<number>;
  /** 活跃分片字节进度快照(仅 downloading_set 中的分片):index → downloaded bytes */
  bytesMap: Map<number, number>;
  /** 终态标记:true 时拒绝合并 downloading delta,防止延迟事件导致幽灵格子 */
  finalized: boolean;
}
```

**4b.** `loadTaskFragments`(line 27-60)内,两处 `set` 调用(merged 分支 line 44-49 与新建分支 line 51-56)补 `bytesMap`:

merged 分支:
```typescript
      next.set(taskId, {
        total: view.total,
        doneSet: mergedDone,
        downloadingSet: snapshotDownloading,
        bytesMap: data.bytesMap, // 保留已有字节快照
        finalized: data.finalized,
      });
```

新建分支:
```typescript
      next.set(taskId, {
        total: view.total,
        doneSet: snapshotDone,
        downloadingSet: snapshotDownloading,
        bytesMap: new Map(),
        finalized: false,
      });
```

**4c.** 改 `mergeFragmentDelta` 签名(line 80-111),加 `fragmentBytes?: { index: number; downloaded: number }[]` 参数,合并到 bytesMap(快照覆盖式 + completedDelta 移除):

```typescript
export function mergeFragmentDelta(
  taskId: string,
  completedDelta: number[],
  startedDelta: number[],
  fragmentBytes?: { index: number; downloaded: number }[],
) {
  setFragmentMap((prev) => {
    const data = prev.get(taskId);
    if (!data) return prev;
    if (
      completedDelta.length === 0 &&
      startedDelta.length === 0 &&
      (!fragmentBytes || fragmentBytes.length === 0)
    )
      return prev;
    const effectiveStarted = data.finalized ? [] : startedDelta;
    const next = new Map(prev);
    const newDone = new Set(data.doneSet);
    const newDownloading = new Set(data.downloadingSet);
    for (const idx of completedDelta) {
      newDone.add(idx);
      newDownloading.delete(idx);
    }
    for (const idx of effectiveStarted) {
      if (!newDone.has(idx)) newDownloading.add(idx);
    }
    // 字节快照:快照覆盖式重建。completedDelta 的分片不在新快照中(已移除)
    const newBytesMap = new Map<number, number>();
    if (fragmentBytes) {
      for (const entry of fragmentBytes) {
        // 跳过已完成的(防御:后端已完成分片不应出现在快照,但前端兜底)
        if (!newDone.has(entry.index)) {
          newBytesMap.set(entry.index, entry.downloaded);
        }
      }
    }
    next.set(taskId, {
      ...data,
      doneSet: newDone,
      downloadingSet: newDownloading,
      bytesMap: newBytesMap,
    });
    return next;
  });
}
```

**4d.** `clearTaskFragmentDownloading`(line 121-129)补清 bytesMap:

```typescript
export function clearTaskFragmentDownloading(taskId: string) {
  setFragmentMap((prev) => {
    const data = prev.get(taskId);
    if (!data || (data.downloadingSet.size === 0 && data.finalized)) return prev;
    const next = new Map(prev);
    next.set(taskId, {
      ...data,
      downloadingSet: new Set(),
      bytesMap: new Map(),
      finalized: true,
    });
    return next;
  });
}
```

- [ ] **Step 5: 实现 downloads.ts — updateProgress 传 fragmentBytes**

在 `frontend/src/stores/downloads.ts` 找到 `mergeFragmentDelta` 调用处,改为传 `payload.fragmentBytes`:

Run: `grep -n "mergeFragmentDelta" frontend/src/stores/downloads.ts`
Expected: 显示调用行

把调用从 `mergeFragmentDelta(id, payload.completedDelta ?? [], payload.startedDelta ?? [])` 改为:

```typescript
mergeFragmentDelta(
  id,
  payload.completedDelta ?? [],
  payload.startedDelta ?? [],
  payload.fragmentBytes,
)
```

- [ ] **Step 6: 运行测试**

Run: `cd frontend && bun run test -- taskFragments-bytes 2>&1 | tail -20`
Expected: 3 个测试 PASS

- [ ] **Step 7: typecheck + lint**

Run: `cd frontend && bun run typecheck 2>&1 | tail -5 && bun run lint 2>&1 | tail -5`
Expected: 无错误,零 lint warning

- [ ] **Step 8: 提交**

```bash
git add frontend/src/types.ts frontend/src/stores/taskFragments.ts frontend/src/stores/downloads.ts frontend/src/stores/__tests__/taskFragments-bytes.spec.ts
git commit -m "feat(frontend): taskFragments store 扩 bytesMap 合并活跃分片字节快照"
```

---

## Task 7: 前端 ChunkMatrix DOM 格子半填充弹簧(FragmentFill 子组件)

**Files:**
- Create: `frontend/src/components/FragmentFill.tsx`
- Modify: `frontend/src/components/ChunkMatrix.tsx`(DOM `<Index>` 渲染处加 FragmentFill + tooltip 修 percent=50)
- Modify: `frontend/src/styles/components/chunk-matrix.css`(加 .chunk-cell-fill)
- Test: `frontend/src/components/__tests__/FragmentFill.spec.tsx`(新建)

**Interfaces:**
- Consumes: Task 6 的 `TaskFragmentData.bytesMap`、`useReducedMotion`
- Produces: `<FragmentFill progress={0..1} reducedMotion={bool} />` 渲染充能条

- [ ] **Step 1: 写失败测试 — FragmentFill 渲染充能条 + reduced-motion 降级**

新建 `frontend/src/components/__tests__/FragmentFill.spec.tsx`:

```typescript
import { describe, it, expect } from "vitest";
import { render } from "@solidjs/testing-library";
import FragmentFill from "../FragmentFill";

describe("FragmentFill", () => {
  it("渲染充能条 transform scaleX 由 progress 决定", () => {
    const { container } = render(() => (
      <FragmentFill progress={0.5} reducedMotion={false} />
    ));
    const fill = container.querySelector(".chunk-cell-fill");
    expect(fill).toBeTruthy();
    // 非降级模式:应有 will-change: transform(由 CSS class 控制,此处仅验证 DOM 存在)
  });

  it("progress=0 时不渲染充能条(空态)", () => {
    const { container } = render(() => (
      <FragmentFill progress={0} reducedMotion={false} />
    ));
    const fill = container.querySelector(".chunk-cell-fill");
    // progress=0 可渲染但 scaleX(0) 不可见,或直接不渲染。此处验证不抛错
    expect(container).toBeTruthy();
  });

  it("reducedMotion 降级:不使用 spring,直接设 transform", () => {
    const { container } = render(() => (
      <FragmentFill progress={0.7} reducedMotion={true} />
    ));
    const fill = container.querySelector(".chunk-cell-fill");
    expect(fill).toBeTruthy();
    if (fill) {
      // 降级模式 transform 应为静态 scaleX(0.7)
      expect((fill as HTMLElement).style.transform).toContain("0.7");
    }
  });
});
```

- [ ] **Step 2: 运行测试,确认失败**

Run: `cd frontend && bun run test -- FragmentFill 2>&1 | tail -20`
Expected: FAIL(`Cannot find module '../FragmentFill'`)

- [ ] **Step 3: 实现 FragmentFill.tsx**

新建 `frontend/src/components/FragmentFill.tsx`:

```typescript
import { Motion } from "@motionone/solid";
import type { JSX } from "solid-js";

interface FragmentFillProps {
  /** 分片已下载比例 [0, 1] */
  progress: number;
  /** 是否减少动画(reduced-motion 降级为静态 transform) */
  reducedMotion: boolean;
  /** 颜色,默认用 token */
  color?: string;
}

/**
 * 分片半填充充能条:在 chunk-cell 内部从左到右填充,弹簧物理驱动。
 *
 * 性能:仅动 transform: scaleX(GPU 合成,零 reflow)。reduced-motion 降级为
 * 静态 transform(无 spring,无 rAF)。父级 chunk-cell 设 overflow: hidden +
 * position: relative,本组件 absolute inset-0。
 *
 * spring 参数:stiffness 300 / damping 30 / mass 0.8(与 DetailPanel 滑入一致)。
 */
export default function FragmentFill(props: FragmentFillProps): JSX.Element {
  const pct = () => Math.max(0, Math.min(1, props.progress));
  const color = () => props.color ?? "var(--color-status-downloading)";

  if (props.reducedMotion) {
    return (
      <div
        class="chunk-cell-fill"
        style={{
          transform: `scaleX(${pct()})`,
          background: color(),
        }}
        aria-hidden="true"
      />
    );
  }

  return (
    <Motion.div
      class="chunk-cell-fill"
      initial={{ transform: "scaleX(0)" }}
      animate={{ transform: `scaleX(${pct()})` }}
      transition={{ type: "spring", stiffness: 300, damping: 30, mass: 0.8 }}
      style={{ background: color() }}
      aria-hidden="true"
    />
  );
}
```

- [ ] **Step 4: 加 CSS — chunk-cell-fill**

在 `frontend/src/styles/components/chunk-matrix.css` 的 `.chunk-cell--pending`(line 110)后加:

```css
/* 分片半填充充能条:absolute 覆盖 chunk-cell,从左到右 scaleX 填充 */
.chunk-cell-fill {
  position: absolute;
  inset: 0;
  border-radius: inherit;
  transform-origin: left center;
  pointer-events: none;
  z-index: 0;
  /* 顶部高光:模拟液面反光 */
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.18);
  will-change: transform;
}

/* 充能条与 chunk-cell 层级:chunk-cell 内容(idle 点阵等)在 fill 之上 */
.chunk-cell--downloading .chunk-cell-fill {
  background: linear-gradient(
    90deg,
    color-mix(in srgb, var(--color-status-downloading) 70%, transparent) 0%,
    var(--color-status-downloading) 100%
  );
}
```

- [ ] **Step 5: 运行测试**

Run: `cd frontend && bun run test -- FragmentFill 2>&1 | tail -20`
Expected: 3 个测试 PASS

- [ ] **Step 6: 实现 ChunkMatrix.tsx DOM 格子加 FragmentFill**

在 `frontend/src/components/ChunkMatrix.tsx` 顶部 import:

```typescript
import FragmentFill from "./FragmentFill";
```

在 DOM `<Index>` 渲染处(line 717-760),每个 chunk-cell 内加 FragmentFill。原 `<div class="chunk-cell" ...>` 内目前为空(自闭合或含 ::after 伪元素)。改为:

```typescript
              <Index each={fragmentIndices()}>
                {(idx) => {
                  const status = createMemo(() => {
                    const data = fragData();
                    const doneSet = data?.doneSet ?? EMPTY_SET;
                    const downloadingSet = data?.downloadingSet ?? EMPTY_SET;
                    const index = idx();
                    if (doneSet.has(index)) return "done";
                    if (downloadingSet.has(index)) return "downloading";
                    return "pending";
                  });
                  // 该分片的字节进度比例(仅在 downloading 状态有值)
                  const fillProgress = createMemo(() => {
                    const data = fragData();
                    if (!data) return 0;
                    const index = idx();
                    if (!data.downloadingSet.has(index)) return 0;
                    return data.bytesMap.get(index) ?? 0;
                  });
                  const isSelected = createMemo(
                    () => selectedIndex() === idx(),
                  );
                  return (
                    <div
                      class="chunk-cell"
                      classList={{
                        "chunk-cell--done": status() === "done",
                        "chunk-cell--downloading":
                          status() === "downloading",
                        "chunk-cell--pending": status() === "pending",
                        "chunk-cell--selected": isSelected(),
                        "chunk-cell--reduced": prefersReducedMotion(),
                      }}
                      data-status={status()}
                      data-index={idx()}
                      role="button"
                      tabIndex={0}
                      aria-label={tr("chunk.tooltip.fragment", {
                        index: idx() + 1,
                      })}
                      onFocus={() => showHover(idx())}
                      onBlur={() => hideHover()}
                      onClick={() =>
                        setSelectedIndex((prev) =>
                          prev === idx() ? null : idx(),
                        )
                      }
                      onKeyDown={(e) => handleKeyDown(e, idx())}
                    >
                      <Show when={status() === "downloading" && fillProgress() > 0}>
                        <FragmentFill
                          progress={fillProgress()}
                          reducedMotion={prefersReducedMotion()}
                        />
                      </Show>
                    </div>
                  );
                }}
              </Index>
```

注意:`fillProgress` 取 `bytesMap.get(index)` 返回的是**字节绝对值**,需转为 [0,1] 比例。但当前 store 不存每片 total bytes(只有 downloaded bytes)。**决定**:FragmentFill 接受字节绝对值时,因不知每片大小,无法算比例。**修正方案**:在 `TaskFragmentData` 额外存 `bytesTotalMap`?过度复杂。**更简方案**:bytesMap 存**比例**而非绝对值——但这需要后端或前端算。

**采用方案 B(前端计算)**:bytesMap 仍存绝对字节(后端原样),前端用 `downloaded / fragmentSize` 算比例。但单分片大小前端不知道。

**最终采用方案 C(诚实降级)**:FragmentFill 的 `progress` 暂用字节绝对值的归一化——用整片预估大小 `fileSize / fragmentsTotal` 作为分母。在 ChunkMatrix 已有 `props.fragmentsTotal`,新增 prop `fileSize`。修改:

ChunkMatrixProps 加 `fileSize?: number | null`。在 `fillProgress` memo 里:

```typescript
                  const fillProgress = createMemo(() => {
                    const data = fragData();
                    if (!data) return 0;
                    const index = idx();
                    if (!data.downloadingSet.has(index)) return 0;
                    const downloaded = data.bytesMap.get(index) ?? 0;
                    if (downloaded === 0) return 0;
                    // 诚实降级:用整片预估大小作分母(可能不精确,但下载中渐增有活感)
                    const total = props.fileSize && props.fragmentsTotal > 0
                      ? props.fileSize / props.fragmentsTotal
                      : 0;
                    if (total <= 0) return 0;
                    return Math.min(1, downloaded / total);
                  });
```

并在 `ChunkMatrix` 调用处(`DetailPanel.tsx:909-914`)传 `fileSize`:

```typescript
                <ChunkMatrix
                  taskId={task()!.id}
                  fragmentsTotal={task()!.fragmentsTotal}
                  fragmentsDone={task()!.fragmentsDone}
                  progress={task()!.progress}
                  fileSize={task()!.fileSize}
                />
```

并在 `ChunkMatrix` 的 `interface ChunkMatrixProps`(line 19-24)加:

```typescript
interface ChunkMatrixProps {
  taskId: string;
  fragmentsTotal: number;
  fragmentsDone: number;
  progress: number;
  fileSize?: number | null;
}
```

- [ ] **Step 7: 修 tooltip 写死的 percent = 50**

在 `frontend/src/components/ChunkMatrix.tsx` 的 `tooltipData` memo(line 618 附近),DOM 模式分支:

```typescript
    const percent = isDone ? 100 : isDownloading ? 50 : 0;
```

改为:

```typescript
    const data2 = fragData();
    const downloaded = isDownloading ? (data2?.bytesMap.get(idx) ?? 0) : 0;
    const total = props.fileSize && props.fragmentsTotal > 0
      ? props.fileSize / props.fragmentsTotal
      : 0;
    const percent = isDone
      ? 100
      : isDownloading && total > 0
        ? Math.min(100, Math.round((downloaded / total) * 100))
        : 0;
```

- [ ] **Step 8: 运行 ChunkMatrix 现有测试 + 新测试**

Run: `cd frontend && bun run test -- ChunkMatrix FragmentFill 2>&1 | tail -20`
Expected: 全 PASS(注意:ChunkMatrix 现有 spec 若断言 percent=50 会失败,需同步更新——见 Step 9)

- [ ] **Step 9: 更新 ChunkMatrix 现有 spec 中断言 percent=50 的用例**

Run: `grep -n "50" frontend/src/components/__tests__/ChunkMatrix.spec.tsx`
Expected: 显示断言行

把断言 `percent` 为 50 的用例改为:基于 `fileSize` 与 `bytesMap` 的真实计算,或改为断言「downloading 状态有充能条 DOM」而非硬编码 50。具体改法依现有 spec 内容定(本 step 在执行时由 Tester agent 据实际 spec 内容调整)。

- [ ] **Step 10: typecheck + lint**

Run: `cd frontend && bun run typecheck 2>&1 | tail -5 && bun run lint 2>&1 | tail -5`
Expected: 无错误

- [ ] **Step 11: 提交**

```bash
git add frontend/src/components/FragmentFill.tsx frontend/src/components/ChunkMatrix.tsx frontend/src/styles/components/chunk-matrix.css frontend/src/components/__tests__/FragmentFill.spec.tsx frontend/src/components/__tests__/ChunkMatrix.spec.tsx frontend/src/components/DetailPanel.tsx
git commit -m "feat(frontend): ChunkMatrix DOM 格子半填充弹簧进度条 + 修 tooltip 写死 50%"
```

---

## Task 8: 前端 Canvas 聚合块渐变深度(>200 分片)

**Files:**
- Modify: `frontend/src/components/ChunkMatrix.tsx`(`drawCanvas` 的块填充逻辑)
- Test: 现有 `ChunkMatrix.spec.tsx` Canvas 相关用例(若 mock canvas)

**Interfaces:**
- Consumes: Task 6 的 `bytesMap`(Canvas 模式需聚合到 block 级)
- Produces: Canvas 块按 block 内活跃分片平均进度画渐变填充深度

- [ ] **Step 1: 写失败测试 — Canvas 块渐变深度由 block 进度决定**

在 `frontend/src/components/__tests__/ChunkMatrix.spec.tsx` 加(Canvas mock 环境):

```typescript
  it("Canvas 聚合块:downloading block 按平均进度画渐变填充", () => {
    // mock canvas getContext 返回 stub
    const fillRectCalls: number[] = [];
    HTMLCanvasElement.prototype.getContext = vi.fn().mockReturnValue({
      setTransform: vi.fn(),
      clearRect: vi.fn(),
      roundRect: vi.fn(),
      fill: vi.fn(),
      beginPath: vi.fn(),
      fillRect: vi.fn(function (this: any, x: number, y: number, w: number) {
        fillRectCalls.push(w);
      }),
      createLinearGradient: vi.fn().mockReturnValue({ addColorStop: vi.fn() }),
      save: vi.fn(),
      restore: vi.fn(),
      clip: vi.fn(),
      stroke: vi.fn(),
    }) as any;

    const { container } = render(() => (
      <ChunkMatrix
        taskId="t-canvas"
        fragmentsTotal={250}
        fragmentsDone={0}
        progress={0}
        fileSize={1000000}
      />
    ));
    // 触发 canvas 绘制(组件 onMount 调 drawCanvas)
    const canvas = container.querySelector("canvas");
    expect(canvas).toBeTruthy();
    // 至少调用了 fillRect(块底色或渐变)
    expect(fillRectCalls.length).toBeGreaterThan(0);
  });
```

- [ ] **Step 2: 运行测试,确认失败**

Run: `cd frontend && bun run test -- "Canvas 聚合块" 2>&1 | tail -20`
Expected: FAIL 或现有逻辑已过(本 task 的关键是改 drawCanvas 填充深度)

- [ ] **Step 3: 实现 — drawCanvas 块填充按平均进度**

在 `frontend/src/components/ChunkMatrix.tsx` 的 `buildBlocks` 函数(line 68-128),为每个 block 计算 `avgProgress`。在 `block.status` 设置后(line 111-125),加:

```typescript
  // 计算每 block 的平均字节进度(仅 downloading 分片)
  const totalBytes = new Array(blockCount).fill(0) as number[];
  if (fragBytesMap) {
    for (const [idx, downloaded] of fragBytesMap) {
      if (idx < 0 || idx >= total) continue;
      const blockIdx = Math.floor((idx * blockCount) / total);
      if (blockIdx < blockCount) {
        totalBytes[blockIdx]! += downloaded;
      }
    }
  }
  const avgProgress = new Array(blockCount).fill(0) as number[];
  for (let i = 0; i < blockCount; i++) {
    const block = blocks[i]!;
    const downloadingInBlock = downloadingCounts[i]!; // 已有
    if (downloadingInBlock > 0 && block.total > 0) {
      // block 内每个分片预估大小 = fileSize / total;block 总预估 = 该值 × block.total
      const perFragSize = fileSize && total > 0 ? fileSize / total : 0;
      const blockExpected = perFragSize * block.total;
      avgProgress[i] = blockExpected > 0
        ? Math.min(1, totalBytes[i]! / blockExpected)
        : 0;
    }
  }
```

注意:`buildBlocks` 当前签名是 `(total, doneSet, downloadingSet)`,需扩为 `(total, doneSet, downloadingSet, fragBytesMap?, fileSize?)`。在 `drawCanvas` 内,为 downloading block 画一个半透明渐变填充(从左到右按 avgProgress):

在 `drawCanvas` 的块遍历(line 385-473)内,`block.status === "downloading"` 分支的块底色绘制后,加渐变深度:

```typescript
      // downloading block 渐变深度(按 block 平均进度)
      if (block.status === "downloading") {
        const blockList2 = blockList;
        const i2 = i;
        const progress = blockProgressFor(i2); // 需 memo 暴露 avgProgress
        if (progress > 0) {
          const fillW = BLOCK_SIZE * progress;
          const grad2 = ctx.createLinearGradient(x, y, x + fillW, y);
          grad2.addColorStop(0, "rgba(53, 221, 226, 0.25)");
          grad2.addColorStop(1, "rgba(53, 221, 226, 0.55)");
          ctx.fillStyle = grad2;
          ctx.beginPath();
          ctx.roundRect(x, y, fillW, BLOCK_SIZE, radius);
          ctx.fill();
        }
      }
```

**wiring**:需把 `avgProgress` 数组通过 memo 暴露给 drawCanvas。在组件内:

```typescript
  const blockProgress = createMemo(() => {
    const data = fragData();
    const bytesMap = data?.bytesMap ?? new Map<number, number>();
    return buildBlockProgress(
      props.fragmentsTotal,
      data?.downloadingSet ?? EMPTY_SET,
      bytesMap,
      props.fileSize,
    );
  });
```

`buildBlockProgress` 为 `buildBlocks` 内 avgProgress 计算的提取函数(返回 `number[]`,index = blockIdx)。需新建此 export 函数。

(本 task 实现较重,执行时由 Coder agent 据现有 drawCanvas 结构精确嵌入,Tester agent 补 canvas mock 测试。)

- [ ] **Step 4: 运行测试**

Run: `cd frontend && bun run test -- ChunkMatrix 2>&1 | tail -20`
Expected: 全 PASS

- [ ] **Step 5: typecheck + lint**

Run: `cd frontend && bun run typecheck && bun run lint`
Expected: 无错误

- [ ] **Step 6: 提交**

```bash
git add frontend/src/components/ChunkMatrix.tsx frontend/src/components/__tests__/ChunkMatrix.spec.tsx
git commit -m "feat(frontend): Canvas 聚合块按平均字节进度画渐变填充深度"
```

---

## Task 9: 前端外壳微调 1 — 侧边栏选中速度线

**Files:**
- Modify: `frontend/src/styles/components/sidebar.css`(`.sidebar-nav-indicator` 选中态动画)
- Modify: `frontend/src/components/Sidebar.tsx`(indicator 仅在 active 时渲染速度线)
- Test: 现有 `Sidebar.spec.tsx`

**Interfaces:**
- Consumes: 现有 `--color-accent-primary`、`useReducedMotion`
- Produces: 选中态 nav item 左侧速度线流动(抽象「赛道」隐喻)

- [ ] **Step 1: 写失败测试 — 选中态 indicator 有速度线动画 class**

在 `frontend/src/components/__tests__/Sidebar.spec.tsx` 加:

```typescript
  it("选中态 nav item 的 indicator 有速度线流动 class", () => {
    const { container } = render(() => <Sidebar />);
    const activeItem = container.querySelector(".sidebar-nav-item.is-active .sidebar-nav-indicator");
    expect(activeItem).toBeTruthy();
    expect(activeItem?.classList.contains("sidebar-nav-indicator--active")).toBe(true);
  });
```

- [ ] **Step 2: 运行测试,确认失败**

Run: `cd frontend && bun run test -- "选中态 nav item" 2>&1 | tail -20`
Expected: FAIL(class 不存在)

- [ ] **Step 3: 实现 — Sidebar.tsx indicator 加 active class**

在 `frontend/src/components/Sidebar.tsx` 的 `NavItem`(line 88-120),`<span class="sidebar-nav-indicator" ... />` 改为:

```tsx
      <span
        class="sidebar-nav-indicator"
        classList={{ "sidebar-nav-indicator--active": p.active }}
        aria-hidden="true"
      />
```

- [ ] **Step 4: 加 CSS — 速度线流动**

在 `frontend/src/styles/components/sidebar.css` 找 `.sidebar-nav-indicator`(或末尾)加:

```css
.sidebar-nav-indicator {
  position: absolute;
  left: 0;
  top: 50%;
  transform: translateY(-50%);
  width: 3px;
  height: 0;
  background: var(--color-accent-primary);
  border-radius: 2px;
  opacity: 0;
  transition: height var(--duration-normal) var(--ease-emphasized),
              opacity var(--duration-normal) var(--ease-standard);
}

.sidebar-nav-indicator--active {
  height: 60%;
  opacity: 1;
}

/* 速度线:active 时顶部光点向下流动(抽象赛道速度隐喻) */
.sidebar-nav-indicator--active::after {
  content: "";
  position: absolute;
  left: 0;
  top: 0;
  width: 100%;
  height: 30%;
  background: linear-gradient(
    180deg,
    var(--color-accent-primary-hover) 0%,
    transparent 100%
  );
  border-radius: inherit;
  animation: sidebar-velocity 1.2s var(--ease-standard) infinite;
}

@keyframes sidebar-velocity {
  0% { transform: translateY(-100%); opacity: 0; }
  20% { opacity: 1; }
  100% { transform: translateY(330%); opacity: 0; }
}

@media (prefers-reduced-motion: reduce) {
  .sidebar-nav-indicator--active::after {
    animation: none;
    display: none;
  }
}
```

- [ ] **Step 5: 运行测试**

Run: `cd frontend && bun run test -- Sidebar 2>&1 | tail -20`
Expected: 全 PASS

- [ ] **Step 6: 提交**

```bash
git add frontend/src/components/Sidebar.tsx frontend/src/styles/components/sidebar.css frontend/src/components/__tests__/Sidebar.spec.tsx
git commit -m "feat(frontend): 侧边栏选中态速度线流动(抽象赛道隐喻)"
```

---

## Task 10: 前端外壳微调 2 — 状态栏速度峰值标记

**Files:**
- Modify: `frontend/src/components/StatusBar.tsx`(sparkline 峰值点标记)
- Modify: `frontend/src/styles/components/status-bar.css`(峰值点样式)
- Test: 现有 `StatusBar.spec.tsx`

- [ ] **Step 1: 写失败测试 — sparkline 有峰值标记**

在 `frontend/src/components/__tests__/StatusBar.spec.tsx` 加:

```typescript
  it("速度 sparkline 标记历史峰值点", () => {
    const { container } = render(() => (
      <StatusBar
        isIdle={false}
        totalSpeed={1024}
        activeCount={1}
        pausedCount={0}
        totalCount={1}
      />
    ));
    const peak = container.querySelector(".sparkline-peak");
    expect(peak).toBeTruthy();
  });
```

- [ ] **Step 2: 运行测试,确认失败**

Run: `cd frontend && bun run test -- "速度 sparkline" 2>&1 | tail -20`
Expected: FAIL(`.sparkline-peak` 不存在)

- [ ] **Step 3: 实现 — Sparkline 组件加峰值点**

先看 `frontend/src/components/Sparkline.tsx` 现有结构:

Run: `cat frontend/src/components/Sparkline.tsx`
Expected: 显示组件实现

在 Sparkline 组件内,计算 `Math.max(...data)` 对应点,渲染一个 `<circle class="sparkline-peak">`。具体实现依现有 Sparkline 结构(若它是 canvas 绘制,则在 canvas 上画点;若是 SVG,则加 circle)。

**若 Sparkline 是 SVG**:在 path 后加:

```tsx
      {peakIndex >= 0 && (
        <circle
          class="sparkline-peak"
          cx={peakX}
          cy={peakY}
          r={2}
        />
      )}
```

**若 Sparkline 是 canvas**:在 draw 末尾画峰值点:

```typescript
  const peakIdx = data.indexOf(Math.max(...data));
  if (peakIdx >= 0) {
    const px = (peakIdx / (data.length - 1)) * width;
    const py = height - (data[peakIdx] / max) * height;
    ctx.fillStyle = "var(--color-accent-primary)"; // 注意 canvas 不能用 var,需 resolveToken
    ctx.beginPath();
    ctx.arc(px, py, 2, 0, Math.PI * 2);
    ctx.fill();
  }
```

(Coder agent 据实际 Sparkline 实现选择路径。)

- [ ] **Step 4: 加 CSS**

在 `frontend/src/styles/components/status-bar.css` 加:

```css
.sparkline-peak {
  fill: var(--color-accent-primary);
  filter: drop-shadow(0 0 3px var(--color-accent-glow-strong));
}

@media (prefers-reduced-motion: reduce) {
  .sparkline-peak {
    filter: none;
  }
}
```

- [ ] **Step 5: 运行测试**

Run: `cd frontend && bun run test -- StatusBar 2>&1 | tail -20`
Expected: 全 PASS

- [ ] **Step 6: 提交**

```bash
git add frontend/src/components/Sparkline.tsx frontend/src/styles/components/status-bar.css frontend/src/components/__tests__/StatusBar.spec.tsx
git commit -m "feat(frontend): 状态栏 sparkline 标记速度峰值点"
```

---

## Task 11: 前端外壳微调 3 — 任务行下载中微型光迹

**Files:**
- Modify: `frontend/src/components/TaskItem.tsx`(下载行左侧光迹)
- Modify: `frontend/src/styles/components/task-item.css`(`.task-item-light-trail`)
- Test: 现有 `TaskItem.spec.tsx`

- [ ] **Step 1: 写失败测试 — 下载行有光迹元素**

在 `frontend/src/components/__tests__/TaskItem.spec.tsx` 加:

```typescript
  it("下载中任务行渲染微型光迹元素", () => {
    const { container } = render(() => (
      <TaskItem
        task={{
          id: "t1",
          url: "https://example.com/a.bin",
          fileName: "a.bin",
          fileSize: 1024,
          downloaded: 512,
          speed: 100,
          status: "downloading",
          progress: 0.5,
          fragmentsTotal: 4,
          fragmentsDone: 2,
          createdAt: "2026-01-01T00:00:00Z",
          savePath: "/tmp/a.bin",
        }}
        index={0}
        isSelected={false}
        isMultiSelected={false}
        isMultiSelectMode={false}
        onClick={() => {}}
        density="comfortable"
      />
    ));
    const trail = container.querySelector(".task-item-light-trail");
    expect(trail).toBeTruthy();
  });
```

- [ ] **Step 2: 运行测试,确认失败**

Run: `cd frontend && bun run test -- "下载中任务行" 2>&1 | tail -20`
Expected: FAIL(元素不存在)

- [ ] **Step 3: 实现 — TaskItem 加光迹元素**

在 `frontend/src/components/TaskItem.tsx` 的根元素内,加条件渲染:

```tsx
      <Show when={props.task.status === "downloading"}>
        <div class="task-item-light-trail" aria-hidden="true" />
      </Show>
```

具体位置:TaskItem 根元素(通常是 `<div class="task-item ...">` 或 `<li>`)的第一个子元素。

- [ ] **Step 4: 加 CSS — 微型光迹**

在 `frontend/src/styles/components/task-item.css` 加:

```css
.task-item-light-trail {
  position: absolute;
  left: 0;
  top: 0;
  bottom: 0;
  width: 2px;
  background: linear-gradient(
    180deg,
    transparent 0%,
    var(--color-accent-primary) 50%,
    transparent 100%
  );
  opacity: 0.7;
  animation: task-trail-pulse 1.6s var(--ease-standard) infinite;
  pointer-events: none;
}

@keyframes task-trail-pulse {
  0%, 100% { opacity: 0.3; transform: scaleY(0.6); }
  50% { opacity: 0.9; transform: scaleY(1); }
}

@media (prefers-reduced-motion: reduce) {
  .task-item-light-trail {
    animation: none;
    opacity: 0.5;
    transform: none;
  }
}
```

- [ ] **Step 5: 运行测试**

Run: `cd frontend && bun run test -- TaskItem 2>&1 | tail -20`
Expected: 全 PASS

- [ ] **Step 6: 提交**

```bash
git add frontend/src/components/TaskItem.tsx frontend/src/styles/components/task-item.css frontend/src/components/__tests__/TaskItem.spec.tsx
git commit -m "feat(frontend): 任务行下载中微型光迹(与详情页矩阵视觉语言一致)"
```

---

## Task 12: 前端全量回归 + 预检

**Files:**
- 无修改,仅验证

- [ ] **Step 1: 前端 typecheck + lint + 测试**

Run: `cd frontend && bun run typecheck && bun run lint && bun run test 2>&1 | tail -20`
Expected: 全 PASS,零 lint warning

- [ ] **Step 2: 前端构建**

Run: `cd frontend && bun run build 2>&1 | tail -10`
Expected: 构建成功

- [ ] **Step 3: 后端全量回归**

Run: `cargo fmt --all -- --check && cargo build --all && cargo nextest run --all && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -15`
Expected: 全通过,零警告

- [ ] **Step 4: 本地 CI 预检**

Run: `cargo fmt --all -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all && cargo deny check && cargo audit && cargo machete && taplo check 2>&1 | tail -15`
Expected: 全通过

- [ ] **Step 5: 无提交(纯验证步)**

---

## 验收标准

1. 下载一个多分片文件(≥ 4 分片),打开详情页,观察 ChunkMatrix:下载中分片格子内有从左到右的半填充充能条(弹簧动效),随字节增长平滑增长;不再只有三级色突变。
2. tooltip 悬停下载中分片,显示真实百分比(非写死 50)。
3. >200 分片时 Canvas 模式块按平均进度画渐变深度。
4. reduced-motion 开启时:弹簧降级为静态 transform,侧边栏速度线、任务行光迹动画消失。
5. 侧边栏选中项左侧有速度线流动动画。
6. 状态栏 sparkline 标记速度峰值点。
7. 任务列表下载中行左侧有微型光迹。
8. `cargo nextest run --all` 全过、`cargo clippy` 零警告、`bun run test` 全过、`bun run lint` 零 warning。

## 风险与缓解

- **风险**:bytesMap 归一化用 `fileSize / fragmentsTotal` 作每片预估分母,当分片大小不均(首尾片)时百分比可能 >100% 或不准确。**缓解**:前端 `Math.min(1, ...)` clamp;完成时直接 100% 覆盖。诚实降级,不影响「下载中渐增」的活感。
- **风险**:Canvas drawCanvas 改动较大(Task 8),可能影响现有 Canvas 测试。**缓解**:Task 8 Step 1 先加失败测试,Tester agent 据 mock 行为调整。
- **风险**:TaskProgress 加字段导致所有字面量构造编译失败(Task 3)。**缓解**:Task 3 集中补全,grep 定位所有构造点。
