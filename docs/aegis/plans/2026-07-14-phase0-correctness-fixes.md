# 实现计划：Phase 0 正确性修复（TDD）

- 日期：2026-07-14
- 基线：`5dd8bc7c37e0440c6ccc85aae8724ab9c6751a62`
- Spec：`docs/aegis/specs/2026-07-14-phase0-correctness-fixes.md`（已批准）
- 策略：严格 TDD；每任务独立实现 + 规格审查 + 质量审查；默认不 commit 除非用户要求

## Plan Basis

- 第一批：F1 AlignedBuf 别名、F2 Unix Statvfs FFI、F3 retry write_buf
- 后续一口气：F4 short write → F5 work-stealing 正确性 → F6 快照水位 → F7 BT → F8 If-Range/镜像
- 非目标：吞吐优化、HLS 产品接线、公网/swarm

```text
Requirement Ready Check: ready（用户批准 Spec）
Change Necessity: code-change
Existence Check: reuse-existing
TDD Route: strict
```

## Files

| 任务 | 文件 |
|---|---|
| F1 | `crates/tachyon-io/src/aligned_buf.rs` |
| F2 | `crates/tachyon-engine/src/storage_adapter.rs`, `crates/tachyon-engine/Cargo.toml` |
| F3 | `crates/tachyon-engine/src/downloader.rs` |
| F4+ | 见后续切片，不并行改同一冲突域 |

## Compatibility

- `AlignedBuf` 公共 API 签名不变；语义收紧（split 后父缓冲不得覆盖已 freeze 区域）
- Unix 磁盘空间探测结果仍为 `Option<u64>`；Windows 路径不变
- retry 行为对外不变，仅消除错误残留

## Tasks

### Task 1 — F1 RED：别名回归测试

1. 在 `aligned_buf.rs` 的 `tests` 中新增：
   - `test_split_freeze_not_aliased_with_parent_writes`
   - 步骤：写 `b"secret"` → `split().freeze()` → 父 `extend_from_slice(b"XXXXXX")` → 断言 frozen 仍为 `b"secret"`
   - 可选：`test_split_advances_offset_keeps_alignment`（split 后 parent.as_ptr 前进且 512 对齐）
2. 运行：`rtk proxy cargo nextest run -p tachyon-io --lib --locked split_freeze_not_aliased -- --nocapture`
3. **期望 RED**：当前实现失败（frozen 被污染）

### Task 2 — F1 GREEN：offset 推进 split

1. 保持 `AlignedBuf::split` 的父 `offset` / `cap` 不变，仅 `pos=0`；前缀产物仍共享 Arc。
2. 新增私有写前隔离 helper：所有安全可写入口（`extend_from_slice`、`as_mut_ptr`、`as_mut_slice`）以 `Arc::get_mut` 判断独占；共享时按原 align 和 cap 新分配，仅拷贝 `[offset..offset+pos)` 已初始化前缀，并将父/child 自身 offset 置 0。
3. `as_mut_slice` 在创建 `&mut [u8]` 前零填 spare `[pos..cap)`，避免将未初始化字节构造成 `u8` slice；`extend_from_slice` 用 checked_add。
4. 更新 Safety / 共享契约注释，明确 Arc 仅管理生命周期、COW 才管理可写排他性。
5. 运行全部 aligned_buf 测试至绿。

验证：`rtk proxy cargo nextest run -p tachyon-io --lib --locked`

### Task 3 — F1 交叉验证

- Spec reviewer：测试是否真覆盖别名；实现是否仍可能 `as_mut_slice` 覆盖 shared 区
- Quality reviewer：Safety 注释是否更新；无新 clippy 警告

### Task 4 — F2 RED：布局/ABI 约束

1. 在 `storage_adapter.rs` unix 测试中加：
   - `size_of` 使用的缓冲类型与 `libc::statvfs` 一致，或
   - 文档化后直接改实现并保留现有 `test_available_disk_space_*` 作为回归
2. 推荐：直接切 `libc`（Task 5）前先确认现有测试在 Windows 通过

### Task 5 — F2 GREEN：libc::statvfs

1. `crates/tachyon-engine/Cargo.toml`：`libc = { workspace = true }` 或 `"0.2"`，`cfg` 可选依赖若 workspace 已有则用 workspace
2. 删除手写 5 字段 `Statvfs` 与手工 `extern "C"`
3. 使用 `libc::statvfs` + `libc::statvfs` 结构体
4. SAFETY：完整结构、路径 CString 有效
5. 验证：`rtk proxy cargo nextest run -p tachyon-engine --lib --locked available_disk_space`

### Task 6 — F2 交叉验证

- 确认无短结构残留；macOS/Linux 字段读取正确（f_bavail * f_frsize）

### Task 7 — F3 RED：retry 残留测试

1. 在 `downloader.rs` 测试中新增场景：
   - MockProtocol attempt1：推送 < WRITE_BATCH_BYTES 数据后 Network 错误
   - attempt2：返回完整正确 range
   - 断言最终文件/MemoryStorage 字节正确
2. 期望：若未 clear，可能出现前缀污染（测试应能在当前代码失败或在故意不 clear 时失败）

若难以构造半缓冲污染：在单元级测「retry 入口必须 clear」的可观察行为——通过 instrument 或在 test-only 钩子验证。优先真实字节断言。

### Task 8 — F3 GREEN：attempt 入口 clear

```rust
// retry loop 内、download_single_fragment 之前
write_buf.as_mut().clear();
```

移除或保留循环外 clear（冗余可接受）。验证定向测试 + 相关 engine 测试。

### Task 9 — F3 交叉验证 + 第一批收口

```bash
rtk proxy cargo nextest run -p tachyon-io -p tachyon-engine --locked
rtk proxy cargo clippy -p tachyon-io -p tachyon-engine --all-targets -- -D warnings
```

更新 `docs/aegis/work/2026-07-14-phase0-correctness-fixes/{20-checkpoint,90-evidence}.md`

### Task 10 — F4 complete-write invariant

1. **RED（Downloader test owner）**：真实 `DownloadTask::run()` full-stream：
   - known size + short storage：必须完整成功、逐字节相等；
   - unknown size + short storage：不得 Completed 但丢尾部；
   - known size source 超出 metadata：必须在越界 chunk 写入前失败，已允许的前缀不变。
2. **RED（Storage adapter test owner）**：故意 overreport 的 `AsyncStorage`：
   - `DynStorage::{write_at,write_at_mut}` 返回 error；
   - `StorageSet::Multi::{write_at,write_at_mut}` 返回 error 而非 `Bytes::slice` panic，且不访问后续 segment。
3. **GREEN**：
   - `execute_full_download` 使用 canonical `write_all_at`；对 known expected_size 与 unknown max_full_stream_bytes 统一做 `pos + chunk_len` 写前边界；
   - 在 `DynStorage` type-erasure 边界验证 `written <= offered_len`；`AsyncStorage` trait 文档明确上界；
   - `write_all_at` 删除错误 min-clamp 语义，并保留防御性上界 error。
4. **交叉验证**：two independent reviewers 检查 BT fallback 例外、cancel/pause 不变量、StorageSet Multi 不 panic。
5. **F4-R3 控制写入准入（用户已选择协作式准入）**：
   - RED：真实 `run()` full-stream 首短写后同步 Pause，Resume 前第二次逻辑写不得启动；另以直接 `execute()` 覆盖 canonical `write_all_at` 门；
   - GREEN：execute 期间保留 `self.control_rx`，外层仅以 clone 观察 Cancel；`write_all_at` 每次新逻辑写入前复用 `wait_control`；
   - 契约：已准入的底层写允许完成，Pause/Cancel 后不再启动新的逻辑写；不承诺撤销已提交 kernel I/O；
   - 非目标：`DownloadTask.state` 的 Pause 可见性、final-EOF 终态优先级、StorageSet 内部每个子写的抢占，均留给独立控制状态机/存储切片。

### Task 11 — F5 work-stealing 安全收敛（Hard-Disable）

设计依据：`docs/aegis/work/2026-07-14-phase0-correctness-fixes/30-f5-design-addendum.md`；用户已选择 **安全收敛**。

```text
Anti-Entropy:
- Path: delete-first for runtime orchestration; compat-exception for public enable_work_stealing field
- Completion semantics: Phase0 F5 = P0 safety mitigation, NOT full feature repair
```

1. **RED（Tester only）**：在 `downloader.rs` 测试中替换 4 KiB 假阳性测试：
   - `test_work_stealing_true_never_splits_static_topology`：`enable_work_stealing=true`，≥3 个、每个 ≥256 KiB 的确定性 range 分片；一个快速、一个慢速；真实 `DownloadTask::run()`；断言 Completed、字节精确、`fragments.len()` 与初始 plan 相等、各 index start/end 不变。
   - 保留/强化 `test_work_stealing_disabled_slow_fragment_still_completes` 作为 false 基线。
2. **GREEN（Implementer）**：仅改 `execute_fragmented_download`：
   - 删除 steal timer/channel/select 分支与 `find_slowest_fragment`/`calculate_split_point` 调用链；
   - `true` 时一次 `warn!(requested=true, active=false, ...)`；
   - 保留静态 dispatcher；
   - **不**删除 `DownloadConfig.enable_work_stealing`、serde、patch、backup carrier；
   - **不**删除公开 `FragmentRecord::try_split`（dormant API）。
3. **配置兼容（core）**：缺字段 default false；显式 true round-trip 仍 true；patch `Some(true)` 仍写入字段。
4. **交叉验证**：规格审查确认危险路径不可达；质量审查确认无第二运行时 owner；完成语义为 bounded mitigation。
5. 完整 WorkUnit/Lease/manifest 重构明确延后 Phase 0.5/1。

### Task 12 — F6 快照 ≤ file durable 水位

设计：`docs/aegis/work/2026-07-14-phase0-correctness-fixes/30-f6-design-addendum.md`

1. **RED**：`OrderingStorage`/`SyncCountingStorage` + 真实分片完成路径；`completed:true` 到达时 `sync_count` 必须 ≥1 且顺序 Write→Sync→Completed。
2. **GREEN**：`download_single_fragment` 在发送 `completed:true` 前对 engine-owned storage 调用 `storage.sync().await?`；`skip_write` 跳过。
3. 零 schema；不改 RecoveryManager / chunk_reader_pool 所有权。
4. 交叉验证：确认 partial/BT 边界非目标被遵守。

### Task 13+ — Phase0 后续

F7 BT block_on/cancel/target ownership → F8 If-Range/镜像（可 Phase0.5）。

## Risks

- `split` 改 offset 后 `as_mut_slice` 仍暴露 cap 区；若 freeze 视图仍共享，父写会进新 offset——正确；勿把 cap 恢复成整块而不检查 strong_count
- F3 测试可能被现有 mock 路径绕过——必须走真实 fragment worker retry 路径
- libc 依赖重复版本：优先 workspace

## Retirement

- 审计 repro 目录可保留作证据；生产代码删除短 Statvfs
- 错误 Safety 注释中「各持有独立 offset 不会重叠」在修复前是假的——修复后更新为真

## Execution

- 模式：主审协调 + 子 Agent 实现/审查（可用时）
- 提交：仅用户要求时再 commit
