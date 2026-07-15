# Phase 0 修复证据

## F1 AlignedBuf

- RED：`split().freeze()` 后 parent 写入污染 frozen；最终又发现 empty split COW 伪指针。
- GREEN：`tachyon-io` lib 149/149；Clippy 0。
- 交叉验证：规格、soundness、empty-view final review Approve。
- 实现：`Arc::get_mut` 写前 COW、full-cap mutable slice 初始化、empty split 不分离真实 allocation、raw pointer 契约补全。

## F2 Unix statvfs

- RED：审计 WSL canary 证明短 5-field struct 被 libc 写出 72B。
- GREEN：手写 ABI/extern 删除；`MaybeUninit<libc::statvfs>`，仅 rc==0 后 assume_init。
- Oracle：一次 native statvfs + 同一完整 struct 的 pure conversion helper，避免 TOCTOU。
- WSL：engine production `cargo check` + clippy（no-default-features）通过；默认 test build 被 WSL 缺 `pkg-config`/`libssl-dev` 阻断，未计为 pass。
- 交叉验证：final F2 review Approve。

## F3 retry write_buf

- RED：真实 `DownloadTask::run` fragmented worker：`stale` + retry → expected 64 / writes 69。
- GREEN：每次真实 attempt 前 clear。
- 验证：partial-stream、auto-retry、retry-exhausted exact tests pass；engine clippy 0。
- 交叉验证：Approve。

## F4 complete-write invariant

### 基础短写

- RED known：16B source 仅写 8B，EOF error；`red-f4-known.txt`。
- RED unknown：Completed 但 data truncate/misaligned；`red-f4-unknown.txt`。
- GREEN：full-stream uses canonical `write_all_at`；known/unknown exact tests + B11 cancellation pass（implementer evidence）。

### 对抗补强

- RED known pre-write boundary：expected 3，`abc` + `def` 先落盘 `abcdef`；`red-f4-known-boundary.txt`。
- GREEN：write-before rejection exact test pass；`green-f4-known-boundary.txt`。
- RED overreport: DynStorage write/write_mut returned `Ok(4)` for 3B; Multi write/write_mut panicked in `Bytes::slice`.
- GREEN：four exact DynStorage/Multi overreport tests pass；`green-f4-*.txt`。
- 设计：type-erasure `DynStorage` enforces `written <= offered`; `write_all_at` retains independent defensive check; known+unknown source ceilings checked before storage write.

### F4-R3：协作式 Pause/Cancel 写入准入

- RED：`rtk proxy cargo nextest run -p tachyon-engine --lib --locked --retries 0 test_run_full_stream_pause_blocks_short_write_retry_until_resume -- --exact`，exit 100；首短写发送 `Pause` 后，测试在 Resume 前观察到第二次 `write_at`，失败位置 `downloader.rs:4265`。
- 根因：`run_inner` execute 段以 `self.control_rx.take()` 把 receiver 移到只观察 Cancel 的外层 watcher，inner execute/write_all_at 因而看见 `None`；即使 receiver 存在，`write_all_at` 每轮补写前也没有 `wait_control` 准入门。
- 用户决策：协作式准入——Pause/Cancel 在下一次逻辑写入前被观察后阻止新写；已获准入、可能已提交的底层 I/O 允许完成；不声称撤销 kernel/spawn_blocking I/O。
- 设计：`30-f4-design-addendum.md` 的 F4-R3；测试后续补直接 `execute()` 同型路径，GREEN 后独立规格与质量复核。

### Deferred to F7

- `protocol_managed_storage` full path duplicate write is confirmed, but preferred filename can diverge from BT StorageFactory target. Do not apply naked skip-write; resolve ownership/target identity under F7.

## F5 work-stealing hard-disable

- 用户决策：安全收敛；完整 WorkUnit/Lease 重构延后 Phase 0.5/1。
- RED：`test_work_stealing_true_never_splits_static_topology` 在旧代码于 steal 路径 `completed_tx.as_ref().unwrap()` panic；`red-f5-true-topology.txt`。
- GREEN：删除 steal timer/channel/select 与慢分片拆分 helper；`true` 仅 `requested=true,active=false` warning + 静态分片。
- 兼容：config/patch/serde/backup 字段保留；core 4 项契约测试锁定缺字段 false / 显式 true / camelCase 序列化 / patch apply。
- topology 双测：true/false 均为 3×256KiB + 初始/终态 start-end-size 锁；`final-f5-work-stealing-pair.txt`。
- 包级：`tachyon-engine` lib 284/284；core+engine all-target/all-feature Clippy 0。
- 最终审查 Approve；完成语义 = **P0 safety mitigation**，不是功能修复。
- dormant：`FragmentRecord::try_split` 公开 API 保留，DownloadTask 不调用。

## Cross-validation protocol

- Tester and Implementer are separate; final reviewers are read-only.
- Cargo/bench operations are serial; environment failures are not presented as passes.
