# F6 Design Addendum — 快照不得领先于文件 durable 水位

- 日期：2026-07-15
- 基线：Phase0 连续修复（F1–F5 已收口）
- 状态：Phase0 最小正确性修复（零 schema 变更）

## 1. 问题

`TaskSnapshot.completed_fragments` / 相关 checkpoint 会在下载文件 `sync_data` 之前被 `RecoveryManager.put_durable` 落盘。断电后 resume 信任 snapshot，跳过可能仅在 page cache 的字节。

错误顺序：

```text
write_at (page cache)
→ FragmentProgress{completed:true}
→ chunk_reader_pool update_snapshot (JSON + file/dir fsync)
→ … 任务结束才 storage.close() → sync_data
```

## 2. 不变量

> **I-F6：** 任意 durable `TaskSnapshot` 中的 `completed_fragments[i]`，必须保证对应分片字节已在同一 crash domain 内通过 `AsyncStorage::sync`（或等价 close/sync_data）进入 durable 状态。

推论：

1. `completed:true` 进度事件是跨层契约；producer（engine）必须在发送前建立 sync barrier。
2. `chunk_reader_pool` 可继续 batch checkpoint；它不得成为文件 sync owner。
3. `RecoveryManager` 只保证 snapshot JSON durable，不感知下载文件。

## 3. Canonical Owner

| 面 | Owner | 文件 |
|---|---|---|
| 数据何时 durable | `DownloadTask` / `StorageSet` | `crates/tachyon-engine/src/downloader.rs` |
| 何时可宣称 completed | `download_single_fragment` 完成路径 | 同上 |
| 何时写 snapshot | `chunk_reader_pool` | `crates/tachyon-app/src/runtime/chunk_reader_pool.rs` |
| snapshot 如何 durable | `RecoveryManager` | `crates/tachyon-store/src/recovery.rs` |
| resume 如何消费 | `inject_resume_snapshot` + `plan` | app/engine（保持信任 I-F6） |

## 4. 推荐方案

**方案 B（推荐）：Engine barrier before `completed:true`**

在 `download_single_fragment` 发送 `FragmentProgress{completed:true}` **之前**：

```text
if !skip_write {
  storage.sync().await?;
}
// 然后才 progress_tx.send(completed:true)
```

- 零 schema
- 不改 app/store API
- 与 “completed ⇒ resume 可跳过” 语义对齐
- 成本：每完成分片一次 fsync（正确性优先；吞吐优化另开 Phase）

**拒绝：**

- 仅靠 resume re-hash 纵深防御而不修写路径
- 把 file sync 塞进 RecoveryManager
- 去掉热路径 durable snapshot
- 完整 2PC + parent dir sync（Phase0 过重）

**Partial 处理（本切片边界）：**

- `partial_fragments` 同构风险存在，但 F6 标题与 Phase0 Spec 聚焦 completed。
- 本切片：**必须**修 completed barrier；partial 记为 residual，可在同文件最小补强（例如 pause 前 sync）但不强制完整 watermark 协议。
- 若实现中 completed barrier 后仍易测 partial 伤害，可追加 “partial checkpoint 前不要求每 chunk sync” 的明确 non-goal。

**BT `skip_write`：**

- 协议自管存储时 engine 未写目标文件；`skip_write=true` 时 **不要** 对 engine storage 做无意义 sync 冒充 barrier。
- BT durable 与 target ownership 属 F7；本切片仅保证 engine-owned write 路径。

## 5. TDD

### RED

1. `OrderingStorage` / `SyncCountingStorage`：
   - `write_at` 递增 write_seq
   - `sync` 记录 sync_seq=write_seq 并 inc sync_count
   - `close` 调 sync
2. 真实单分片 `DownloadTask::run` 或 `download_single_fragment` 路径：
   - 在收到 `completed:true` 时断言 `sync_count >= 1` 且顺序为 Write… → Sync → Completed
3. 旧代码期望：completed 到达时 sync_count==0（任务未结束）

### GREEN

- 在 completed 事件前 `storage.sync().await?`
- 失败则分片失败/可重试，不得发送 completed
- 任务结束 `close` 仍保留（可二次 sync；允许冗余，不为 “去重 fsync” 牺牲清晰性）

### 回归

- 既有 resume / multi-fragment / short-write / pause-admission 测试保持绿
- skip_write 路径不因多余 sync 行为错误失败

## 6. Anti-Entropy

```text
Anti-Entropy Declaration:
- Deletion Class: code-retirement of “completed means only page-cache write”
- Old Path: send completed immediately after write_all_at success
- New Canonical Owner: engine completion path with sync barrier
- Expected Preserved Behavior: resume skip completed; snapshot schema; app checkpoint batching
- Expected Retired Behavior: durable snapshot claiming completed without file sync
- External Boundary Touched: no schema; behavior contract of completed progress strengthened
- Source-of-Truth Data Risk: none (strengthens durability)
- User Confirmation Required: no

Retirement Decision:
- Path: delete-first for unsafe completion ordering
- Why: internal ordering bug; no external API field removed
```

## 7. 完成语义

- Phase0 F6 = **崩溃一致性安全缓解**：completed snapshot 不再领先 file durable 水位
- 不宣称吞吐提升
- 不完成 partial watermark 完整协议
- 不完成 parent-dir sync / 2PC
- 不完成 BT ownership（F7）

## 8. 验证

```bash
rtk proxy cargo nextest run -p tachyon-engine --lib --locked --retries 0 sync_before_completed
rtk proxy cargo nextest run -p tachyon-engine --lib --locked --retries 0
rtk proxy cargo clippy -p tachyon-engine --all-targets --all-features --locked -- -D warnings
```
