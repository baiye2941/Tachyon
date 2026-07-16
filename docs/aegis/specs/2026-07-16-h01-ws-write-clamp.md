# Spec Brief：H-01 work-stealing 写边界裁剪 + steal 队列满回滚

## 范围
1. `download_single_fragment`：每次 `flush_batch` 前按当前 `effective_end` **裁剪** batch/`write_buf`；`pos > effective_end` 时丢弃缓冲并停止写。
2. work-stealing：`steal_tx.try_send` 失败时 **回滚** `try_split`（恢复原 fragment end/size/effective_end，pop 新 fragment）。
3. 单元测试：`clamp_write_to_effective_end` 边界；`try_send` 满回滚后原分片区间恢复。

## 非目标
- 完整 owner epoch / 已提交 `write_at` 的取消
- 默认开启 work-stealing
- snapshot 与动态 fragment 边界恢复契约（C-01/C-02 全量）

## TDD
strict
