# 检查点：BT Session 生命周期（2026-07-23 最终）

## TodoCheckpointDraft

- **已完成（全部本轮）：**
  - Slice 3A 全部四个 cache-miss add 路径接入 coordinator（probe + download_range_stream + download_full + download_full_stream），singleflight gate + layout-failure cleanup。
  - `stop_and_remove_torrent` 退休：coordinator 存在时委托 `request_background_cleanup_for`（tracked, non-blocking），无 coordinator 回退保留旧 detached 路径。
  - engine 378/378 GREEN（`stop_and_remove_torrent` 签名不变，engine 无需改动）。
  - Observer 3B lazy capture：`peer_stats_snapshot` 首次 live 时缓存 stats，cleanup 后 live 不可取时返回最后捕获值。251/251 protocol tests + 378/378 engine tests + clippy 零警告（lib）。
  - S-02a2 App typed outcome GREEN + 独立 Reviewer approve。
  - S-02b store reservation GREEN：`TaskNamespaceReservation`、`reserve_task_namespace`、reserved 变体、normal API `ReservationActive` 拒绝、restore `max(tombstone,disk)+1` + post-write tombstone clear。98/98 store tests + 6/6 s02b tests。
  - S-02 export fail-closed：`export_backup_inner` 在 `unsupported_schema` 非空时返回 `UpgradeRequired` 而非 warn-then-export。
- **活动阶段：** 无更多未开始的原子切片在本轮范围内。

## 验证证据

```text
# Magnet protocol（含 observer lazy capture + 全四路径 + 并发回归）
251 passed, 0 skipped

# Magnet engine
378 passed, 0 skipped

# Magnet clippy (lib)
零警告

# S-02 store（含 reservation + restore strict durable）
98 passed, 0 skipped

# S-02a2 app（含 export fail-closed）
task_store 14/14, commands load_recovered 1/1, export 2/2
```

## 残留未完成（需后续 RED）

**Magnet 3A+:**
- `HandleCache`/`SessionOpsGate` 与 coordinator 双轨并存（coordinator 路径下 add 不经 ops_gate；stop_and_remove coordinator 路径不经 ops_gate）。
- engine `DownloadTask::run` tail cleanup 仍调 `stop_and_remove_torrent`（现已委托 coordinator，但 engine 未直接持有 `BtCleanupAction`）。
- AlreadyManaged 兼容性、Quarantined/generation/lease、engine latch 3C、CLI 3D、App 3E。

**S-02c+:**
- S-02c facade admission gate（`TaskStore::admit_shared/admit_exclusive`）。
- S-02d ordinary lifecycle mutation 严格保存/补偿。
- S-02d1 runtime cancellation/session failure 设计 preflight。
- S-02d2/d3 download lifecycle typed failure。
- S-02e delete/undo exclusive operation。
- S-02f RawValue backup/import preflight 与补偿。

## ResumeStateHint

恢复时读本文件 + 父 design §6/§7/§8/§11 + S-02 follow-up §3/§7。两 worktree 均有未提交 production diff；不得当作可接受基线或跳过新 RED。
