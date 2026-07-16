# Spec Brief：P0-3 分片 completed 前 durable sync

## 范围
1. **分片路径**：`download_single_fragment` 在发送 `completed: true`（上层 checkpoint）前对引擎写入路径调用 `storage.sync()`（`skip_write`/protocol_managed 跳过，BT 自有 piece 写盘语义）。
2. **整块路径**：`execute_full_download_once` 在标记 fragment/task Completed 前 `storage.sync()`。
3. **测试**：CountingSyncStorage 断言分片完成前至少一次 sync。

## 非目标
- 每 batch flush 都 fsync（Flush Storm）
- openat2 / 目录 fsync / store Durable 契约深改
- BT-17 piece-hash 深度交叉

## TDD
strict
