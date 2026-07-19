# 进度链路四项修复计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (or superpowers:executing-plans). 步骤使用 checkbox (`- [ ]`) 语法跟踪。

**Goal:** 修复续传进度回退、broadcast Lagged 丢 delta、整块路径无进度；明确 250ms 契约并降低丢帧风险。全程 TDD，测试 Agent 与实现 Agent 分离，多 Agent 交叉验证。

**Architecture:**  
1) `ChunkReader` 在 `PlanComplete` 时从 `TaskStore` 快照种子化 `total_downloaded` + `frag_bytes`（不改 `FragmentProgress` 线格式，避免跨 crate 协议破环）。  
2) `subscribe_progress` 在 `Lagged` 时用 `task_repository` + `fragment_state_store` 合成权威全量 resync（`completed_delta=done_set`、`started_delta=downloading_set`）。  
3) `execute_full_download_once` 按写入字节调用既有 `report_progress`，结束发 completed。  
4) broker 容量上调 + 注释澄清 250ms 为兜底间隔；前端 `updateProgress` 对 downloading 态做单调保护作为纵深防御。

**Tech Stack:** Rust / tokio / tachyon-app / tachyon-engine / Vitest(frontend)

---

## 约束 (Global Constraints)

- 注释/提交信息中文，标识符英文，无 emoji
- clippy 零警告；本任务相关 nextest 必须绿
- **禁止**跳过 TDD：先红灯测试，再最小实现
- **禁止**同一 Agent 既写测试又写生产实现（项目 multi-agent-engineering 规则）
- 不扩大范围到无关重构
- 不主动 `git commit`，除非用户明确要求
- 现有测试不得无故删除/削弱

## 测试接缝 (Seams)

| Seam | 文件 | 观测 |
|------|------|------|
| S1 ChunkReader PlanComplete 种子化 | `chunk_reader_pool.rs` | `task.downloaded` / `frag_bytes` 回调 / 后续 Chunk 不双重计数 |
| S2 Lagged resync | `progress_commands.rs` | Lagged 后 emit 含全量 done/downloading delta |
| S3 整块进度 | `downloader.rs` | full-stream 路径 `progress_tx` 收到增量 Chunk |
| S4 前端单调 | `downloads.ts` | downloading 时 downloaded 不回退 |

---

### Task 1: 续传进度回退 — 失败测试 (Tester)

**Files:**
- Modify: `crates/tachyon-app/src/runtime/chunk_reader_pool.rs` (tests only)

- [ ] **Step 1: 写红灯测试**

在 `chunk_reader_pool.rs` tests 模块新增：

1. `plan_complete_seeds_downloaded_from_snapshot`  
   - 预置 TaskInfo.downloaded=750, file_size=1000, fragments_total=4  
   - TaskStore 写入 snapshot: completed=[0,1], partial={2: 50}, downloaded=750, fragment sizes 隐含 250+250+50  
   - 发送 `PlanComplete { total:4, completed_indices:[0,1], initial_concurrency:2 }`  
   - **断言**: 处理完后 `task.downloaded == 750`（不得被清零）  
   - **断言**: `task.fragments_done == 2`  
   - **断言**: `task.progress` 约 0.75  

2. `plan_complete_seeds_partial_bytes_no_double_count`  
   - snapshot: completed=[0], partial={1: 100}, downloaded=350, file_size=500 (两片 250)  
   - PlanComplete completed=[0]  
   - 再发 `Chunk { index:1, fragment_downloaded:150, completed:false }`  
   - **断言**: `task.downloaded == 400`（350 + 50，不是 350+150）

3. 回调侧（可选但推荐）: PlanComplete 后 `on_progress` 的 fragment_bytes 含 partial index=1 downloaded=100

- [ ] **Step 2: 跑测试确认 RED**

```bash
cargo nextest run -p tachyon-app -- plan_complete_seeds --exact
```

期望: FAIL，因当前 PlanComplete 把后续 downloaded 从 0 累加或保持但 frag_bytes 空导致双重计数。

- [ ] **Step 3: 写 report** 到 `.superpowers/sdd/reports/task-1-tests-report.md`（仅测试，无生产改动）

---

### Task 2: 续传进度回退 — 实现 (Coder)

**Files:**
- Modify: `crates/tachyon-app/src/runtime/chunk_reader_pool.rs` (production `PlanComplete` 分支)

- [ ] **Step 1: 最小实现**

在 `FragmentProgress::PlanComplete` 分支：

1. 现有 `fragment_state_store.init` / `completed` 集合逻辑保留  
2. 从 `task_store.load_snapshot(&task_id)` 读取（与项目其它路径一致，可同步；若 clippy/异步规范要求则 `spawn_blocking`）  
3. 若有 snapshot:  
   - `frag_bytes = snap.partial_fragments`  
   - `total_downloaded = snap.downloaded`  
   - 若 `snap.downloaded == 0` 且 completed 非空，仍以 0 起步（不编造 size）  
4. 若无 snapshot: fallback `total_downloaded = task_repository` 中现有 `downloaded`  
5. 写回 task: `downloaded`, `fragments_done=completed.len()`, `fragments_total=total`, 重算 `progress`  
6. `on_progress` 回调带上 `frag_bytes` 快照（delta=None 或保持现有 None）  
7. 后续 Chunk 逻辑不变（依赖 frag_bytes 种子避免双重计数）

- [ ] **Step 2: 跑测试确认 GREEN**

```bash
cargo nextest run -p tachyon-app -- plan_complete_seeds
cargo nextest run -p tachyon-app -- chunk_reader
```

- [ ] **Step 3: report** `.superpowers/sdd/reports/task-2-impl-report.md`

---

### Task 3: Lagged 丢 delta — 失败测试 (Tester)

**Files:**
- Modify: `crates/tachyon-app/src/commands/progress_commands.rs` (tests)  
- 可能需要抽出可测的 `handle_progress_recv` / `build_lagged_resync_event` 纯函数

- [ ] **Step 1: 抽接缝（若尚不可测）**  
  将 Lagged 恢复逻辑设计为纯函数：

```rust
pub(crate) fn build_lagged_resync_event(
    task_repository: &TaskRepository,
    fragment_state_store: &FragmentStateStore,
) -> ProgressEvent
```

每个任务：  
- 标量字段来自 TaskInfo  
- `completed_delta` = `done_set` 全量（有序）  
- `started_delta` = `downloading_set` 全量  
- `fragment_bytes` 可空（本任务不强制）

- [ ] **Step 2: 红灯测试**

1. `lagged_resync_includes_all_done_indices`  
   - repo 有 task t1 Downloading  
   - fragment_state_store: total=4, done={0,2}, downloading={1}  
   - 调用 `build_lagged_resync_event`  
   - 断言 completed_delta 含 0,2；started_delta 含 1  

2. `lagged_resync_empty_when_no_frag_state`  
   - 无 frag state 时 delta 为空 vec，标量仍来自 task

- [ ] **Step 3: RED 验证 + report** `task-3-tests-report.md`

---

### Task 4: Lagged 丢 delta — 实现 (Coder)

**Files:**
- Modify: `crates/tachyon-app/src/commands/progress_commands.rs`
- Modify: `crates/tachyon-app/src/projection/progress_broker.rs`（容量/注释）

- [ ] **Step 1: 实现**

1. 实现 `build_lagged_resync_event`  
2. `subscribe_progress` spawn 时 clone `fragment_state_store`  
3. `RecvError::Lagged(n)`:  
   - warn 保留  
   - `let snap = build_lagged_resync_event(...)`  
   - emit `progress-update` 全量（或非空 delta）  
   - `last_snapshot = snap`（关键：避免后续 delta 基于陈旧基线）  
4. `PROGRESS_BROADCAST_CAPACITY`: 64 → 256  
5. 注释澄清：`AGGREGATOR_INTERVAL_MS=250` 是无脏通知时的兜底 tick，不是最小发送间隔；Lagged 恢复保证 delta 最终一致

- [ ] **Step 2: GREEN + report** `task-4-impl-report.md`

```bash
cargo nextest run -p tachyon-app -- lagged_resync
```

---

### Task 5: 整块路径进度 — 失败测试 (Tester)

**Files:**
- Modify: `crates/tachyon-engine/src/downloader.rs` (tests only)

- [ ] **Step 1: 红灯测试**

`full_download_reports_chunk_progress`：  
- 协议 `supports_range=false`，`download_full_stream` 返回多块 Bytes（如 3×100）  
- 挂 `progress_tx` capacity 足够  
- 跑 `execute`/`run` 至完成（或只跑 full path）  
- **断言**: 收到至少 1 条 `Chunk { completed:false, fragment_downloaded > 0 }`  
- **断言**: 最终有 `Chunk { completed:true }` 或等价终态进度（优先 completed:true，与分片路径一致）  
- fragment_index 对整块路径为 0

参考已有 B11 full_download 测试的 mock 协议搭法。

- [ ] **Step 2: RED + report** `task-5-tests-report.md`

---

### Task 6: 整块路径进度 — 实现 (Coder)

**Files:**
- Modify: `crates/tachyon-engine/src/downloader.rs` (`execute_full_download_once`)

- [ ] **Step 1: 最小实现**

在每次成功 `write_all_at` 且 `pos` 更新后：

```rust
Self::report_progress(0, pos, &self.progress_tx);
```

在循环正常结束后、`complete_download_fast` 前：

```rust
if let Some(tx) = &self.progress_tx {
    let _ = tx.try_send(FragmentProgress::Chunk {
        fragment_index: 0,
        completed: true,
        fragment_downloaded: pos,
    });
}
```

注意：整块路径 `fragments` 可能为空或单元素——与现有 `first_mut` 完成逻辑对齐，index 固定 0。

- [ ] **Step 2: GREEN**

```bash
cargo nextest run -p tachyon-engine -- full_download_reports_chunk_progress
cargo nextest run -p tachyon-engine -- B11
```

- [ ] **Step 3: report** `task-6-impl-report.md`

---

### Task 7: 前端单调保护 — 测试+实现（可同一前端 Agent，因单文件小改；仍先红后绿）

**Files:**
- Modify: `frontend/src/stores/downloads.ts`
- Modify: `frontend/src/stores/__tests__/downloads*.ts`（若无则新建或挂靠现有 store 测试）

- [ ] **Step 1: 红灯**  
  `updateProgress` 在 status=downloading 时，若 payload.downloaded < 当前 downloaded，保留较大值；progress 同步不回退。  
  status 变为 pending/failed/cancelled 时允许重置（不强制 max）。

- [ ] **Step 2: 实现最小分支 + GREEN**

```bash
cd frontend && bun test downloads
```

- [ ] **Step 3: report** `task-7-frontend-report.md`

---

### Task 8: 交叉验证 + 回归 (Reviewer)

- [ ] rust-reviewer / 代码审查员：对照本计划 4 项根因，确认均有测试锁住  
- [ ] 全量相关包：

```bash
cargo nextest run -p tachyon-app -p tachyon-engine
cargo clippy -p tachyon-app -p tachyon-engine --all-targets -- -D warnings
cd frontend && bun test
```

- [ ] 输出 `.superpowers/sdd/reports/final-progress-bugs-review.md`  
- [ ] 更新 `.superpowers/sdd/progress.md` ledger

---

## 执行顺序

```
T1(test resume) → T2(impl resume)
T3(test lagged) → T4(impl lagged)   # 可与 T1/T2 并行（不同文件）
T5(test full)   → T6(impl full)     # 可与上并行
T7 frontend
T8 review
```

T1∥T3∥T5 测试可并行；实现不可并行改同一文件。

## 明确不做

- 不改 `FragmentProgress` 线格式（除非 snapshot 方案被证不足）  
- 不把 aggregator 强行改为硬 250ms 最小间隔（只修 Lagged 语义 + 容量）  
- 不改 BT piece truth / protocol_managed 跳过策略  
- 不提交 git（除非用户要求）
