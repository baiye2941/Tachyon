# 实施计划：S-01 / S-02 / S-04 审计确认修复

- **目标**：按经用户确认的设计规格，严格 TDD 修复 S-01、S-02、S-04；不扩大到审计报告其余项。
- **架构**：`tachyon-core → tachyon-protocol / tachyon-store → tachyon-app`；前端仅通过 Tauri DTO 与配置 patch 交互。
- **技术栈**：Rust 2024、Tokio、Serde、Tauri v2、SolidJS、Bun。
- **权威/基线引用**：`docs/aegis/specs/2026-07-22-audit-s01-s02-s04-design.md`；`docs/architecture.md` §2–4；`AGENTS.md`；`.claude/rules/multi-agent-engineering.md`；`58bc93923fe93031330589c96afc22ac55c23a6c`。
- **兼容边界**：旧 peer 配置保留但默认不使用受限项；future snapshots 必须受保护且所有破坏性工作流 fail-closed；PlanComplete 的 seed/callback/channel 顺序保持。
- **验证**：严格 Tester → Coder → Tester → Reviewer；每片先记录实际 RED，再转 GREEN；最终 workspace 与前端全量验证。

## Plan Basis

三项已由独立审计员和红队确认：

- S-01：`crates/tachyon-protocol/src/magnet.rs:619-630,1067-1090,1332-1351,1499-1519,1651-1661,1745-1755`。
- S-02：`crates/tachyon-store/src/recovery.rs:223-431` 与 `crates/tachyon-app/src/commands/task_commands.rs:1459-1596`。
- S-04：`crates/tachyon-app/src/runtime/chunk_reader_pool.rs:277-382`。

## BaselineUsageDraft

- Required baseline refs: 设计规格、架构文档、项目规则、冻结源码。
- Acknowledged before plan: 全部已读取。
- Cited in plan: 本文 File Map 和各 slice 路径。
- Missing refs: 无；librqbit 8.1.1 源码已本地核对 `initial_peers` 直接转 stream，未承诺上限。
- Decision: ready for user review.

## Requirement Ready Check

- Requirement source refs: 用户的“验证报告正确性、正确则 TDD 修复或优化、多 Agent 交叉验证”请求；用户三项显式策略选择；设计规格。
- Acceptance / verification: 每项设计 §3.6 / §4.4 / §5.3；精确 RED/GREEN；独立审查；最终门禁。
- Open blockers: 等待修订设计的独立复审和用户批准后才执行。
- Decision: needs-user-approval.

## Change Necessity

- User-visible need: 防止 magnet 导向受限网络、旧客户端破坏未来任务快照、同步磁盘读占用 Tokio worker。
- No-change / non-code option: 报告/文档无法阻止输入跨库、快照重写或 worker 阻塞。
- Why code change is necessary: 三项均是运行时边界行为。
- Minimum change boundary: 下列 slice owner 文件；不得新增跨层依赖或平行 owner。
- Decision: code-change（待用户批准设计后）。

## Complexity Budget

| Artifact | Current pressure | Plan governance |
|---|---:|---|
| `crates/tachyon-core/src/config.rs` | 4581 lines | 只添加字段、分类 helper、相邻测试；不建 config framework。 |
| `crates/tachyon-protocol/src/magnet.rs` | 3911 lines | 单一 private collection/add boundary；若生产新增 >100 行，暂停。 |
| `crates/tachyon-store/src/recovery.rs` | 1048 lines | 单一 header guard 与结果类别；不复制 batch loops。 |
| `crates/tachyon-app/src/runtime/chunk_reader_pool.rs` | 1812 lines | 一个 narrow loader seam；不抽象 general executor。 |

## File Map

### S-01

- `crates/tachyon-core/src/safety/url_safety.rs`、`crates/tachyon-core/src/lib.rs`：无 feature 例外的 BT peer 分类 API 与 re-export（若 protocol 需访问）。
- `crates/tachyon-core/src/config.rs`：`MagnetConfig` / `MagnetPatch` / default / serde / patch；旧配置保留语义测试。
- `crates/tachyon-protocol/src/magnet.rs`：所有 cache-miss add 路径的 canonical collection 与唯一 librqbit peer 输入边界。
- `crates/tachyon-app/src/commands/config_commands.rs`（仅若现有 config rebuild/validation 测试需要补契约）：保存成功后已有 `magnet_changed` rebuild 行为的回归。
- `frontend/src/types.ts`、`frontend/src/components/settings/SettingsPanel.tsx`、`frontend/src/components/settings/tabs/MagnetTab.tsx`、`frontend/src/i18n/locales/zh-CN.ts`、`frontend/src/i18n/locales/en-US.ts`：DTO、draft、patch、toggle、双语风险提示。
- 相邻现有 Rust `#[cfg(test)]` 模块及现有 SettingsPanel/MagnetTab test pattern：Tester 唯一拥有测试资产。

### S-02

- `crates/tachyon-store/src/recovery.rs`：header guard、future result category、所有 store-level mutation guard、tests。
- `crates/tachyon-app/src/task_store.rs`：传播 `unsupported_schema` 而不伪装为 corrupt。
- `crates/tachyon-app/src/lib.rs`、`crates/tachyon-app/src/commands/mod.rs`：startup recovery warning/result contract（仅找到实际消费 result 的位置后修改）。
- `crates/tachyon-app/src/commands/task_commands.rs`：export/import/delete workflow fail-closed 与 app-layer tests。

### S-04

- `crates/tachyon-app/src/runtime/chunk_reader_pool.rs`：narrow loader helper、production spawn_blocking wiring、test-only hook 和确定性 tests。

## Slice 1 — S-01 BT peer input boundary

### Tester owns

- 仅编辑 tests：core safety/config tests、protocol magnet collector tests、必要 app config test、frontend component/type tests；不能改 production code。
- 对已批准但不存在的 helper/API，允许先得到编译 RED；一旦接口声明落地，必须再获得行为 RED。

### RED tests

1. 无 test-harness 例外的分类：受限 IPv4/IPv6 与公网地址。
2. 旧含受限 `peerAddrs` 配置仍 deserialize/validate 成功，但默认 collection 不输出；`allowPrivatePeers=true` 才输出。
3. URI/config 稳定去重、32+32 来源配额、剩余容量补足、总 64、端口 0/非法项排除。
4. Protocol cache-miss paths 都交由同一 collection；测试不启动真实 swarm。
5. frontend 读取/草稿/保存 patch/toggle/双语 hint。

### RED commands

Tester 先用实际测试名逐个运行，形式如下：

```bash
cargo nextest run -p tachyon-core --lib -- <new_core_test> --exact
cargo nextest run -p tachyon-protocol --features magnet --lib -- <new_protocol_test> --exact
cargo nextest run -p tachyon-app --lib -- <new_config_test> --exact
cd frontend && bun run test -- <SettingsPanel-or-MagnetTab-test>
```

有效 RED 必须是缺失批准接口的编译错误或现有可编译行为的断言失败；不得是 0 tests、网络错误或真实 BT swarm 失败。

### Coder owns

- 仅编辑 S-01 production files；不得改 Tester assertions。
- core 复用单一 IP range rule source，protocol 不复制 CIDR table。
- `add_magnet_to_session` 的 caller 不能传裸 `Vec<SocketAddr>`；所有 four cache-miss calls 进入同一 collector。
- frontend 改动必须与 core camelCase contract 完整同步。

### GREEN / regression commands

```bash
cargo nextest run -p tachyon-core
cargo nextest run -p tachyon-protocol --features magnet
cargo nextest run -p tachyon-app --lib -- <affected_config_test> --exact
cargo build -p tachyon-core
cargo build -p tachyon-protocol --features magnet
cargo fmt --all -- --check
cargo clippy -p tachyon-core --all-targets --all-features -- -D warnings
cargo clippy -p tachyon-protocol --all-targets --all-features -- -D warnings
cd frontend && bun run typecheck && bun run lint && bun run test
```

### Reviewer approval criteria

- No HTTP test-harness loopback exception leaks to BT policy.
- No direct/later cache-miss peer bypass; no new protocol/env configuration owner.
- Existing peer strings remain stored; default policy only removes them from runtime collection.
- UI accurately says opt-in, LAN/self-hosted only, later connections only.
- New logic stays within complexity budget.

## Slice 2 — S-02 future schema full fail-closed

### Tester owns

- Store recovery tests and app task-store/startup/backup/delete/import tests only.
- Raw JSON fixtures use independent temp dirs; no user data/config directory.

### Mandatory design preflight before S-02 RED

Coder and Reviewer must first map the exact linearization boundary available from `RecoveryManager.progress_lock` through `TaskStore`, `TaskService::delete_task`, `export_backup_inner`, and `import_backup_inner`. No app code begins until the plan proves one of:

1. a TaskStore operation executes protected-record scan plus snapshot mutation while the same store mutation lock remains held; or
2. the operation is architecturally unable to race and that claim has direct evidence.

A command-side `keys()` scan followed by an unlocked delete/write is forbidden. The actual task delete owner is `crates/tachyon-app/src/service/task_service.rs`; backup/import owner is `crates/tachyon-app/src/commands/task_commands.rs`.

### RED tests

1. Streaming header parsing: missing schema (legacy), current, current unknown field, non-object, malformed/duplicate `schemaVersion`, null/string/negative/overflow, future header.
2. Future load returns `Unsupported`; no sensitive raw content in error.
3. save/update/restore/remove preserve raw bytes; a guard-passes-but-durable-write-fails case retains tombstone and blocks stale save.
4. Every RecoveryManager batch API has explicit behavior; new result APIs return legal records plus separate `unsupported_schema`, never `corrupt_keys`.
5. export/delete/overwrite import/non-overwrite conflict/future backup input fail before config bytes, repository, raw snapshots, tombstone, temporary backup or local files mutate.
6. Existing TaskRecord/revision/tombstone/valid backup paths remain green.

### RED commands

```bash
cargo nextest run -p tachyon-store --lib -- <new_future_schema_store_test> --exact
cargo nextest run -p tachyon-app --lib -- <new_future_schema_app_test> --exact
```

### Coder owns

- Store recovery/result-model production code and actual app result consumers/destructive workflow code only.
- Header guard uses an explicit streaming top-level object parser and precedes every serde fallback.
- `Unsupported` must be a typed `std::io::ErrorKind::Unsupported`; no string matching.
- Restore deletes tombstone only after durable write succeeds in the same lock.
- App destructive workflows use the approved locked/preflight operation and await writes; no `.ok().flatten()`, warning-only remove failure, detached save, or unlocked scan-then-mutate path.
- Do not add `deny_unknown_fields`, raw JSON passthrough, separate recovery manager, or unbounded backup adapter.

### GREEN / regression commands

```bash
cargo nextest run -p tachyon-store
cargo nextest run -p tachyon-app
cargo build -p tachyon-store
cargo build -p tachyon-app
cargo fmt --all -- --check
cargo clippy -p tachyon-store --all-targets --all-features -- -D warnings
cargo clippy -p tachyon-app --all-targets --all-features -- -D warnings
cargo llvm-cov -p tachyon-store --ignore-filename-regex "(test_harness|iocp|winio|iouring)" --fail-under-regions 90 --summary-only
```

### Reviewer approval criteria

- Future header is streaming-classified before TaskSnapshot/TaskRecord fallback.
- All required destructive paths use a proven linearization boundary and fail before mutation.
- Future data never becomes corrupt log/UI semantics; normal legacy/revision behavior remains.
- No unbounded app-level backup adapter, duplicate schema owner, or scan-then-mutate TOCTOU path.
