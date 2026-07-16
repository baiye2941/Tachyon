# Spec Brief：H-05 快照 revision CAS + 删除 tombstone

## 范围
1. `TaskSnapshot.revision: u64`（serde default 0，schema 5）
2. `RecoveryManager`：`progress_lock` 覆盖 `save_task_snapshot` / `remove_task` / `update_snapshot`
3. Save CAS：`snapshot.revision < existing.revision` → 拒绝；成功则 `revision = existing+1`
4. Delete：锁内删键 + 写 tombstone；之后 `revision <= tombstone` 的 save 拒绝（防复活）
5. App 路径 full-save 合并时拷贝 existing.revision；`TaskStore` 暴露语义
6. 测试：逆序 save、remove 后旧 save

## 非目标
- 取消 fire-and-forget 全部改为 await（CAS 已防乱序覆盖）
- io_uring cancel
- H-06 pool TOCTOU

## TDD
strict
