# Evidence — P0-6/7/8 residual: ops_gate

## Problem

UI `probe_filename` 在 probe 后 `stop_and_remove_torrent`（后台 pause/delete），
而用户可能立刻开始下载 `add_magnet_to_session`。两者并发时可能：

- delete 未完成时 add 拿到半关闭 / AlreadyManaged 脏状态
- 或 add 成功后被迟到的 delete 误杀

## Fix

- `SessionOpsGate = Arc<DashMap<String, Arc<Mutex<()>>>>` 按 magnet URL 串行
- `with_magnet_session_op(gate, url, fut)` 公共 helper
- `BtSession.ops_gate()` 与 `handle_cache` 一并注入所有生产 `MagnetProtocol`
- `add_magnet_to_session` 与 `stop_and_remove` 后台 cleanup 均持同一 URL 锁

## Tests

```
cargo nextest run -p tachyon-protocol -- magnet::tests
# 59/59 pass（含 test_with_magnet_session_op_serializes）

cargo clippy -p tachyon-protocol -p tachyon-engine -p tachyon-app --features magnet --all-targets -- -D warnings
```

## Wire sites

- `BtSession::{ops_gate, new}`
- `DownloadTask` pure magnet + hybrid
- `probe_filename_inner` `.with_ops_gate(session.ops_gate())`
