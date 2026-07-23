# 证据包：S-02 future schema 全失败关闭（2026-07-23 最终）

## Baseline

- Worktree：`C:/Users/白夜/.config/aegis/worktrees/Tachyon/audit-s02-fail-closed`
- Branch：`feat/audit-s02-fail-closed`
- Baseline commit：`58bc93923fe93031330589c96afc22ac55c23a6c`

## 本轮完成

### S-02a store-local streaming classifier（此前 GREEN）

### S-02a2 App typed outcome 传播（本轮 GREEN）

- `AppError::{UpgradeRequired, InvalidSnapshot, Io}` + `map_recovery_error`
- `load_recoverable_with_warnings`/`load_all` 3-tuple 暴露 `unsupported_schema`
- `load_recovered_tasks` 返回 `StartupRecovery`，只插入合法任务
- 独立 Reviewer approve

### S-02b store reservation + restore strict durable（本轮 GREEN）

- `RecoveryError::ReservationActive`
- `TaskNamespaceReservation<'a>`（manager identity + nonce + Drop 释放匹配 active）
- `reserve_task_namespace()`：扫描 task_ keys via header classifier；future/invalid 不创建 reservation
- reserved 变体：`load_reserved`/`save_reserved`/`update_reserved`/`remove_reserved`/`restore_reserved`
- normal API（save/restore/load/load_all/remove/recover_pending/update）active reservation 期间返回 `ReservationActive`
- `restore_task_snapshot`：`max(tombstone,disk)+1` 写入 revision；tombstone 仅在 `put_durable` 成功后清除
- `FileStore::write_entry` directory `sync_all` error 传播（非 warn）

### S-02 export fail-closed（本轮 GREEN）

- `export_backup_inner` 在 `unsupported_schema` 非空时返回 `UpgradeRequired`，不创建不完整 backup

## GREEN 证据

```text
CARGO_TARGET_DIR=target-s02a2 cargo nextest run -p tachyon-store --lib
98 passed

CARGO_TARGET_DIR=target-s02a2 cargo nextest run -p tachyon-store --lib s02b
6 passed

CARGO_TARGET_DIR=target-s02a2 cargo nextest run -p tachyon-app --lib -E 'test(task_store::) + test(commands::tests::load_recovered) + test(export)'
17 passed

CARGO_TARGET_DIR=target-s02a2 cargo clippy -p tachyon-store --lib -- -D warnings
零警告
```

## 残留未完成（S-02c+）

- S-02c：`TaskStore` facade admission gate（`admit_shared`/`admit_exclusive`）
- S-02d：ordinary lifecycle mutation 严格保存/补偿
- S-02d1：runtime cancellation/session failure 设计 preflight
- S-02d2/d3：download lifecycle typed failure
- S-02e：delete/undo exclusive operation
- S-02f：RawValue backup/import preflight 与补偿
- ProtectedSnapshot/StartupRecovery 未 Serialize（UI upgrade notice 未接入 IPC）

## 限定

- S-02a + S-02a2 + S-02b + export fail-closed 已 GREEN。
- **不是完整 S-02 closure。**
- 任一新增修复必须先由独立 Tester 再写 RED。
