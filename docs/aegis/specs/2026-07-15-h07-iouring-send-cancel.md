# Spec Brief：H-07 io_uring fixed-buffer send 取消泄漏

## 范围
1. `submit_read` / `submit_write`：`cmd_tx.reserve().await` 取得 permit **后**才 `mark_submitted`，再 `permit.send`（非 await）
2. reserve 等待期间取消 → guard `submitted=false` → Drop 回收槽位
3. send 失败（receiver 关闭）→ 显式 `reclaim_unsent` 回收槽位
4. 测试：未提交 Drop 回收；模拟 reserve 后失败 reclaim；注释更正「send 前 mark 保守泄漏」语义

## 非目标
- 内核 `AsyncCancel` / CQE drain 完整 cancel 协议
- driver `submit_and_wait` 改 spawn_blocking（M-01）
- engine write_all_at 与 storage 的 generation quiesce

## TDD
strict
