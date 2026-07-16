# Spec Brief：H-06 ConnectionPool host 信号量 cleanup TOCTOU

## 范围
1. `cleanup_idle_hosts`：仅当 `Arc::strong_count(sem) == 1`（仅 map 持有）且全部许可可用时才删除
2. `host_semaphore`：统一走 `entry.or_insert_with`，去掉 get-then-insert 竞态窗口
3. 测试：持有 idle host 的 Arc clone 时 cleanup 不得删除；删除后 re-acquire 不得使 per-host 实际并发超过配置

## 非目标
- 真正的 TCP 连接池复用
- H-07 io_uring fixed-buffer cancel

## TDD
strict
