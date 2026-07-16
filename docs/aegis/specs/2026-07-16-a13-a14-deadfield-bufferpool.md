# Spec Brief：A-13 死字段 + A-14 BufferPool 热重建

## 目标

1. **A-13（delete-first）**：删除 `DownloadTask.bt_session` 死字段（`#[allow(dead_code)]`，构造后无读取）；Session 仅在构造期用于创建 `MagnetProtocol`/`bt_fallback`。
2. **A-14（部分）**：`max_concurrent_tasks` / `max_concurrent_fragments` 变更时热重建 `BufferPool`（与 `ConnectionPool` 同模式：`Arc<RwLock<Arc<BufferPool>>>`），新任务拿到新容量；旧任务持有旧 Arc 自然释放。
3. **A-13 诚实**：`TaskInfo.retry_count` 文档标明「当前恒 0，未接入引擎分片 attempt 聚合」；本轮不改 schema。

## 非目标

- 完整 `ConfigApplier` / `ApplyMode` 字段矩阵
- `DownloadTask` 构造器矩阵大拆分 / 文件物理拆分（A-12）
- `retry_count` 真实引擎事件聚合
- ChunkReaderPool worker 数热调

## TDD

- `test_a13_download_task_has_no_bt_session_field`：编译期保证（字段移除后通过编译）
- `test_a14_update_config_rebuilds_buffer_pool`：patch 更大并发后 `buffer_pool.capacity` 变化且为新 Arc
- 既有 buffer_pool / build_download / magnet 测试保持绿
