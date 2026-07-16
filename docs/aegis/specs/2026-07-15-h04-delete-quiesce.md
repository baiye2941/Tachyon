# Spec Brief：H-04 Delete 等待下载 I/O quiesce

## 范围
1. `delete_task_inner`：Cancel → `wait_for_handle`（带超时）→ 再删文件/快照/仓库 → cleanup
2. `wait_for_handle`：超时后 abort，并再 await 一小段以尽量 drain；同时清理 command channel
3. 测试：注入 sleep JoinHandle，断言 delete 至少等待其完成；超时路径仍可返回

## 非目标
- H-05 snapshot revision 全序
- io_uring fixed-buffer cancel drain
- Undo Cancel/Delete 完整 generation 语义

## TDD
strict
