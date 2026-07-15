# Review Intake — F1/F4

## F1 final review

- F1-R1 zero-size split COW：接受并修复。RED 证明空 view 被错误 detach 至 `0x200`；修复为 cap=0 不 COW、真实 allocation 空 slice；149/149 + final review Approve。
- F1 MSRV 1.85：验证后为依赖图（ICU ≥1.86）与跨文件既有 `is_multiple_of` 的独立 repo governance drift；记录，不伪称本 F1 已修复。

## F4 review intake

### 已验证、接受（同一 complete-write invariant）

1. **F4-R1 known-size pre-write boundary**：`execute_full_download` 仅对 unknown size 限制 `pos + chunk_len`。known-size source 可以先写超过 metadata size，EOF 才报错，污染/扩展输出。接受；补真实 full-stream RED 测试，写前拒绝。
2. **F4-R2 overreported write count**：`write_all_at` 将 `written > remaining.len()` 的非法 backend 返回累加到 pos/metrics，只对 `Bytes` slice 做 min clamp。`StorageSet::Multi` / `write_at_mut` 内部还会 `slice(written..)` panic。接受；这是 canonical write contract，不是额外功能。补 tester-owned regression，Single 必须 error，Multi 不得 panic。

### 待独立核验

3. **protocol_managed_storage full path**：现有 fragmented path skip-write，但 full path未见；需要追 Magnet/ByteStream storage ownership后决定是否属于 F4 或独立 BT slice。

### F4-R3（控制写入准入）

4. `run_inner` 的 `control_rx.take()` 后，execute、full-stream 和 worker 看见 `None`；首短写同步发出 Pause 后，第二次补写在 Resume 前启动。真实 `run()` RED 已以 `test_run_full_stream_pause_blocks_short_write_retry_until_resume` 确认。
5. 用户已选择协作式准入：Pause/Cancel 被每个新逻辑写的准入点观察后，不启动后续逻辑写；已获准入的底层 I/O 可完成。修复必须保留 task receiver，外层 Cancel watcher 用 clone，并让 `write_all_at` 在每轮补写前复用 `wait_control`。
6. 仍延后：`DownloadTask.state` 的 Pause/Resume 可见性、final-EOF 与 Pause/Cancel 终态优先级，以及撤销已提交 kernel I/O；这些需要独立控制状态机设计，不以 F4 局部补丁假装解决。

## F5 work-stealing hard-disable

- 用户决策：安全收敛。
- 设计：`30-f5-design-addendum.md`。
- 接受：删除 runtime steal 编排；`true` warning + 静态路径；保留 public config/backup carrier。
- 拒绝：现在完整重构；删除配置 API；normalize/reject true 导致 config/backup 破坏。
- 完成语义：bounded safety mitigation / feature repair deferred。

## Decision

`accept-F5-hard-disable-safety-mitigation; defer-full-workunit-lease-rewrite-to-phase0.5-1`
