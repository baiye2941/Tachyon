# Spec Brief:第一批 P0 修复(C-03 supervisor ABA + H-04 cancel quiesce + BT-08 防回归)

## 背景
审计 `Document/PI/Tachyon-Deep-Audit-2026-07-14` 残留 P0 项。经探索确认:
- C-03 未修(supervisor 无 generation,cleanup 无条件 remove,start_download 覆盖旧 handle 不 abort)
- H-04 部分修(delete_task_inner 已调 wait_for_handle,cancel_task_inner 未等待)
- BT-08 已修(magnet.rs:818-826 加 wait==MAX 短路),但无防回归测试

## 范围与用户决策

### C-03 supervisor ABA(方案 A:轻量 abort)
- `download_supervisor.rs:start_download` 开头:`if let Some((_, old)) = self.handles.remove(task_id) { old.abort(); }` + `command_channels.remove` + `command_locks.remove`
- 不引入 generation(场景已足够:同 task_id 同时只应一个活跃 task_fn)
- 兼容性:`start_download` 签名不变;调用方无感知

### H-04 cancel 也 quiesce
- `task_commands.rs:cancel_task_inner` 在 send Cancel 后调 `wait_for_handle(task_id, CANCEL_QUIESCE_TIMEOUT)`
- 新增常量 `CANCEL_QUIESCE_TIMEOUT: Duration = Duration::from_secs(5)`(短于 DELETE_QUIESCE_TIMEOUT=15s,cancel 通常更快收敛)
- 超时则 abort + 2s grace(wait_for_handle 内部已有逻辑)
- 与 delete 路径对称

### BT-08 防回归测试
- 补 RED→GREEN 测试:无 peer + peer_wait=0(映射 MAX)+ finite stall → 应返回 Err(Timeout),不永久循环
- 验证现有 magnet.rs:818-826 短路逻辑

## 不变量
- C-03:start_download 后旧 handle 必定 abort(不漂在 runtime);cleanup 不删新 session 的控制面
- H-04:cancel 返回后旧 task 必定 quiesce 或 abort(不继续写盘/联网)
- BT-08:peer_wait=0 + 无 peer 不永久循环

## 非目标
- C-03:不引入 generation token(方案 B,后续若需精准防 ABA 再升级)
- H-04:不修改 delete 路径(已修);不修改 task_service 层(依赖调用方 quiesce)
- BT-08:不修改 magnet.rs 实现(已修,仅补测试)

## TDD
严格 RED→GREEN→REFACTOR,垂直切片。

## 验证
```bash
cargo nextest run --all
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```
