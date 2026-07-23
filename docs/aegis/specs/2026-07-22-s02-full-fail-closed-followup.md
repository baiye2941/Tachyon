# 设计补充：S-02 future schema 全失败关闭的线性化边界

- **目标**：在既有 `RecoveryManager` / `TaskStore` / task command 的所有权内，实现 future schema 的读、写、删、备份和导入全失败关闭；消除 scan-then-mutate TOCTOU，并修正 restore tombstone 顺序。
- **已选范围**：仅提供**进程内**严格保护：store reservation capability、`TaskStore` facade admission gate、失败补偿。**不承诺跨崩溃事务、目录级原子替换或本地文件删除可回滚。**
- **架构**：`tachyon-store` 是 schema 分类、reservation、snapshot mutation 与 durable 写入的唯一 owner；`tachyon-app` 只消费 typed outcome，并通过 `TaskStore` facade 进入受保护操作。不得新增 snapshot manager、JSON schema adapter 或 app 侧 header parser。
- **技术栈**：Rust 2024、Serde JSON（`RawValue`）、Tokio、文件型 `KvStore`。
- **权威/基线引用**：`docs/aegis/specs/2026-07-22-audit-s01-s02-s04-design.md` §4；`AGENTS.md`；`.claude/rules/multi-agent-engineering.md`；`docs/architecture.md`；冻结基线 `58bc939`。
- **兼容边界**：无 `schemaVersion` 的 legacy `TaskRecord`、`schemaVersion <= SNAPSHOT_SCHEMA_VERSION`、revision/tombstone 与正常 backup 均保持；future record 永不被旧客户端重写、删除或静默漏备份。future 是“有效但需要更高版本”，不是 corrupt。
- **验证**：严格 Tester → Coder → Tester → Reviewer；future 负向用例断言 raw bytes、repository、config、tombstone、临时备份和本地文件均未变化；普通 I/O 失败断言补偿结果或 `RepairRequired`，而非伪造事务成功。

## 1. 触发原因与本设计必须覆盖的路径

原 S-02 的“header guard + fail-closed”不足以保护真实调用链：

1. `RecoveryManager::restore_task_snapshot` 当前先移除 tombstone 再写入；durable write 失败时，旧 revision 有机会复活已删任务。
2. `serde_json::Value` / Map 会折叠重复 `schemaVersion`，不能兑现“重复 header 是 InvalidData”。
3. 仅在 app 先 `keys()` / `load()` 再 mutation 是 TOCTOU；token/epoch 复查但不保留 capability 也不足。
4. 下列路径当前把 load/save error 当作 `None`、warning 或后台可忽略失败：
   - `inject_resume_snapshot`；
   - `probe_and_save_metadata`；
   - `commands::persist_task_snapshot`；
   - `TaskService::{create_task, persist_task_state, pause_task, resume_task, cancel_task}`；
   - tag、显示顺序（`order`）及其他 ordinary lifecycle mutation 的持久化路径；
   - `TaskService::{delete_task, undo_delete_task}`；
   - `chunk_reader_pool` 的 `PlanComplete` seed 和全部 checkpoint；
   - `fragment_commands` 的 terminal snapshot fallback；
   - `TaskStore` 的 `save/restore/load/load_recoverable/load_all/remove/update` wrappers。
5. `import_backup_inner` 先反序列化 `Backup<TaskSnapshot>`，重复 key 已丢失；随后又先改 config/repository，并以 detached save 或 warning 吞掉失败。
6. `FileStore::write_entry` 的 directory `sync_all` 当前只 warning；这不满足 restore strict durable path。

因此实施完成前，Reviewer 必须逐项证明：future `Unsupported` 没有经过 `.ok().flatten()`、`and_then(|r| r.ok())`、warning-and-continue、repository fallback 或 detached save 变成 missing/corrupt。

## 2. Canonical schema 模型与 typed outcome

### 2.1 唯一 header classifier

`RecoveryManager` 持有唯一私有 streaming top-level-object visitor；所有 raw snapshot（磁盘读取和 backup 导入）必须经过它：

```text
raw JSON
  -> streaming object visitor（逐字段；计数 schemaVersion；其余字段 IgnoredAny）
  -> non-object / duplicate schemaVersion / null / string / negative / overflow
       = InvalidData，禁止任何 fallback
  -> missing schemaVersion = legacy candidate
  -> schemaVersion > CURRENT = Unsupported(ProtectedSnapshot)
  -> schemaVersion <= CURRENT = TaskSnapshot parse；必要时既有 TaskRecord fallback
```

禁止先反序列化为 `Value` / Map；禁止 future 或 invalid header 后尝试 `TaskRecord` fallback。错误和日志仅含最小的 key/version 诊断，不含 raw JSON、URL 或保存路径。

### 2.2 结果与错误合同

`tachyon-store` 新增并从 `lib.rs` 导出的最小类型：

```rust
pub struct ProtectedSnapshot {
    pub key: String,
    pub found_version: u32,
    pub supported_version: u32,
}

pub struct RecoveryResult {
    pub tasks: Vec<TaskSnapshot>,
    pub corrupt_keys: Vec<String>,
    pub unsupported_schema: Vec<ProtectedSnapshot>,
}
```

所有单 key snapshot API 返回可区分的 store error：`Unsupported(ProtectedSnapshot)`、`InvalidData`、普通 `Io`、`ReservationActive`、`InvalidReservation`。具体枚举命名可随既有错误风格调整，但不得再只靠格式化字符串区分 `Unsupported`。

store error 的 app 映射不属于 S-02a。紧随其后的 **S-02a2** 才由 `TaskStore` / `AppError` / startup consumer 建立下列 typed outcome：

- `UpgradeRequired { found_version, supported_version }`：可展示“已隔离，需要较新版本”；
- `InvalidSnapshot`：无效 header/JSON；
- 普通 I/O error；
- `RepairRequired { operation, primary, compensation }`：主 I/O 已有副作用且补偿也失败。

S-02a 只产出 store 的 `RecoveryResult` 和可区分 store error，绝不修改 `tachyon-app`、不声明 app 已消费 outcome，也不改变 startup。S-02a2 完成前，future 不得因尚未接入的 startup 路径而静默消失；实施者必须先保留并显式审计现有 startup 的可见诊断。上述 app outcome 也不属于 S-04 已完成的 blocking-read 修复，不能倒过来声称 S-04 现有 `PlanComplete` fallback 已能识别 `Unsupported`。

### 2.3 外部传入 snapshot 也必须受保护

`save_task_snapshot`、`restore_task_snapshot`、reserved save/restore，以及任何从 backup 得到的 `TaskSnapshot`，先验证 incoming `schema_version`：

- `> SNAPSHOT_SCHEMA_VERSION`：`Unsupported`，无 write、无 tombstone 变化；
- raw 输入的 malformed/duplicate header：`InvalidData`，无 write、无 tombstone 变化；
- legacy/current：进入既有 revision 规则。

因此调用方自行构造的 future `TaskSnapshot` 不能绕过“先读磁盘 header”的保护。

## 3. Store reservation：真实 capability，不是 scan + epoch

### 3.1 Reservation API 形状

`RecoveryManager` 增加一个 namespace reservation；公开给 app 的入口只经 `TaskStore` facade 暴露：

```text
RecoveryManager::reserve_task_namespace()
  -> 在 store 内 scan 全部 task_ key + 同一 header classifier
  -> 遇 Unsupported/InvalidData：不创建 reservation，直接返回 typed error
  -> 全部可操作：创建 TaskNamespaceReservation

TaskNamespaceReservation
  - manager identity
  - 不可预测 nonce
  - 仅内部构造、不可伪造；不暴露原始 nonce
  - Drop 仅在 identity + nonce 匹配 active reservation 时 release

RecoveryManager::{load,save,update,remove,restore}_reserved(&reservation, ...)
  -> 每次验证 manager identity、nonce 和 active 状态
```

reservation state 属于 `RecoveryManager`，不是 app 的 epoch 或 boolean。普通 snapshot API（含 read、batch read、save、update、remove、restore）在活跃 reservation 期间必须取得 `ReservationActive`，不能先 scan 后照常执行。reservation 所有者只能调用 `*_reserved` 变体；不可通过普通 API 旁路。

### 3.2 线性化与锁规则

- 所有 store snapshot operation 固定顺序为：`progress_lock` → reservation-state mutex。`reserve_task_namespace` 在持有 `progress_lock` 时完成 scan/classification 并登记 active capability，之后才释放 lock。
- 普通 operation 也必须先取得 `progress_lock` 再检查 active reservation，避免“先检查无 reservation、后排队等待 progress_lock”穿透 reservation。
- reserved operation 在同一 `progress_lock` 内重新验证 capability 后才读/写/删；token 绝不只是预检时的 epoch。
- `TaskNamespaceReservation` 保存的是可跨 await 存活的 state/nonce，不保存 `std::sync::MutexGuard`。任何 `std::sync::MutexGuard` 只覆盖同步 store 临界区，绝不跨 `.await`。
- `Drop` 仅释放匹配的 active reservation；它不执行 I/O、也不替代调用方的补偿。access object 不得被 detached task 持有；所有 reserved store call 必须 await 完成后才能离开操作。

这只线性化本进程的 `RecoveryManager`。现有 `FileStore` OS directory lock 仍负责拒绝第二个进程打开同一 store；本设计不将 reservation 宣称为跨崩溃或跨进程事务。

### 3.3 Restore 与 strict durable write

`restore_task_snapshot` 在同一个 `progress_lock` 中依次执行：

1. incoming snapshot 与磁盘 raw header 的 strict guard；
2. 读取 disk revision 和 process-local tombstone revision；
3. 使用 `max(tombstone_revision, disk_revision) + 1` 作为 restore 写入 revision；不得信任传入 revision；
4. 走 restore 专用 strict durable write：serialize、temp write、file fsync、rename、directory fsync 都成功；
5. **仅在步骤 4 成功后**移除 tombstone。

任一步失败均保留 tombstone。`FileStore::write_entry` 的 strict/durable 分支必须将 directory `sync_all` error 传播，而非 warning 后返回 `Ok(())`。`delete_tombstones` 仍仅是**进程内、非崩溃**的旧写防线；它不是 delete/restore 跨崩溃事务日志，也不保证本地文件恢复。

## 4. `TaskStore` facade admission gate（S-02b 后的 S-02c）

紧随 store-only S-02a 的 S-02a2 拥有 `crates/tachyon-app/src/task_store.rs`、`crates/tachyon-app/src/commands/mod.rs` 中的 `AppError` / startup consumer，以及这些入口的 app tests；它只把 S-02a 已有的 store 分类和错误传播到 app。reservation capability 由 S-02b 建立后，S-02c 才在同一 facade 上增加 admission gate。不得把 runtime session failure、`PlanComplete` 传播或 ordinary lifecycle repository 顺序偷渡进 S-02a2 或 S-02c。

### 4.1 API 形状与所有权

在 `crates/tachyon-app/src/task_store.rs`，`TaskStore` 自身封装一个 Tokio owned `RwLock` admission gate；不得另建裸 `AppState` lock。最小 API 形状如下：

```text
TaskStore::admit_shared(self: &Arc<Self>) -> TaskStoreShared
TaskStore::admit_exclusive_task_namespace(self: &Arc<Self>)
  -> Result<TaskStoreExclusive, AppError>

TaskStoreShared::{load, save, update, load_recoverable, load_all, ...}
TaskStoreExclusive::{load_reserved, save_reserved, update_reserved,
                     remove_reserved, restore_reserved, scan_reserved, ...}
```

- `TaskStoreShared` 持有 owned read admission；正常读取、普通 save/update、startup、export、download lifecycle、checkpoint 和 fragment view 都经它进入。
- `TaskStoreExclusive` 持有 owned write admission 与 store reservation；仅 delete、undo delete、import 使用。它的 drop 释放 admission，内含 reservation 的 drop 再释放 store capability。
- access object 的实际同步 store 调用由自身封装进 awaited `spawn_blocking`；app 不取得 `RecoveryManager`、raw token 或“未 admission 的 TaskStore method”。这避免 async 调用方跨 await 持 `std::sync::MutexGuard`。
- 旧裸 wrappers 要删除或降为 facade 私有实现；任何测试也只能从 shared/exclusive access 进入，不能保留 production bypass。

### 4.2 顺序与禁止重入

固定顺序：**TaskStore admission →（exclusive 时）store reservation → 短时 repository/config lock → store/local-file work**。调用 admission 前必须释放 repository/config guard；调用 `spawn_blocking` 前也必须释放 DashMap ref 与 `tokio::MutexGuard`。

禁止：

1. 在已持有 `TaskStoreShared/Exclusive` 时再次调用 `admit_*`；不支持升级、降级或嵌套 admission。
2. 在 exclusive operation 内调用普通 `TaskStore` wrapper；只能使用同一个 `TaskStoreExclusive` 的 reserved method。
3. 在 reservation 外 scan task namespace 后再 delete/import；也不得直接访问 `RecoveryManager`。
4. 持 store `progress_lock`、reservation-state mutex、repository/config guard 或任何 `std::sync::MutexGuard` 进入 `.await`。

Tokio write admission 排队后，新的 shared admission 不得越过它；exclusive delete/undo/import 因而不会与普通 checkpoint/save 交错。此 gate 只约束 snapshot workflow，不替代任务运行、config 或 repository 的既有业务锁。

## 5. 调用路径合同

### 5.1 Startup、普通读取与展示

- startup `load_recoverable_with_warnings` 使用 `TaskStoreShared`，保留合法任务、记录 corrupt，单独收集并上报 `unsupported_schema`；future raw bytes 不变。
- `TaskStore::load_all` / export 在 shared admission 内检查 `unsupported_schema`；任一 future 在创建临时备份文件前返回 `UpgradeRequired`。现有 corrupt-export warning 政策显式保留，不能把 future 混入其中。
- `fragment_commands::{get_task_fragments_inner, load_snapshot_total}` 改为传播 `Result`（Tauri command 同步 await）；future 必须返回 upgrade-required，不得合成 empty view。

### 5.2 Ordinary lifecycle mutation：shared admission、严格写入与 repository 合同

`pause_task`、`resume_task`、`cancel_task`、tag 增删、显示顺序调整（`move_task` / `reorder_tasks`）、`TaskService::create_task` 及其 command/sniffer/hub 入口、`persist_task_state`、`persist_task_snapshot` 和任何 `persist*` helper 都是 ordinary lifecycle mutation。它们不是 delete/import 的 exclusive namespace operation，但同样不得让 future guard 后的 snapshot 写入失败静默丢失。

每个路径必须在后续 dedicated slice 中满足下列二选一合同，并有逐路径测试：

1. **preflight/shared admission + strict save before repository commit**：先取得 `TaskStoreShared`，在改变 repository/config/运行态可见状态前完成 incoming/load guard 与 strict snapshot save；或
2. **repository compensation**：若既有业务顺序必须先改 repository，必须保存原 repository material；strict save 失败时恢复 repository，恢复失败返回 `RepairRequired`。

测试必须证明 `Unsupported`/`InvalidData` 时 repository、config、任务状态、tags/order 和 raw snapshot 不变；普通 I/O 时要么 repository 已补偿，要么显式 `RepairRequired`。`TaskService::create_task` 的去重、并发计数、repository insert 与初始 snapshot save 也必须落在该合同内，不能成为 future guard 的旁路。此为 process-local strict protection，不新增持久化事务承诺。

### 5.3 Download lifecycle 与 checkpoint

以下路径均以 typed `Result` 传播 `Unsupported`/`InvalidData`；它们不得用 missing、repository seed、warning 或 fire-and-forget 代替：

| 路径 | 必须行为 |
|---|---|
| `inject_resume_snapshot` | 保留 `spawn_blocking`，但返回 `Result`。`None` 才是无快照；future/invalid/I/O 由 `DownloadSession::run` 在 probe/run 前停止本次会话并表面化错误。 |
| `probe_and_save_metadata` | 先构造 candidate snapshot 并经 shared strict save 成功，再提交 metadata 到 repository；save 必须 awaited。future/invalid 在 repository/persistent mutation 前失败。 |
| `commands::persist_task_snapshot` | 改为返回并 await `Result`；不 detached save。构造 desired snapshot 后严格写入，必要时只在普通 I/O 失败后按第 6 节补偿内存状态。 |
| `TaskService::persist_task_state` | 改为返回 `Result` 并由 cancel/tag/order 等调用方传播。不得对 `load_snapshot` 用 `.ok().flatten()`；candidate 与现有 snapshot 的合并、strict save、repository commit 必须按保护顺序完成。 |
| `chunk_reader_pool::PlanComplete` | **不得在本设计直接实施传播。** 仅记录为下列 S-04/S-02 blocking preflight 的候选 read-set。现有 `None` fallback 与 `Ok(Err(e))` warning fallback 不能被重述为已支持 `Unsupported`。 |
| `chunk_reader_pool` retry/completed/partial/final checkpoint | 在 session-failure design 获批后，每个 update 走 shared access；`Unsupported`/`InvalidData` 才能定义为 reader 终止条件，不能仅 warning 后消费后续事件；普通 I/O 仍记录失败但不得伪称已 checkpoint。 |

**S-04/S-02 blocking preflight：runtime cancellation / session failure ownership。** 在任何 `PlanComplete Unsupported` 或 checkpoint typed failure 向 runtime 传播前，必须建立 dedicated design slice；不得仅把 `oneshot::Sender<()>` 改成携带结果就声称已经停止运行 session。该 slice 的 read-set 至少包括 `ChunkReaderJob::done_tx`、`run_chunk_reader`、pool dispatcher/worker、`wait_chunk_reader_done`、`DownloadSession::run`、`DownloadTask::run` 的进度 sender/关闭语义、`TaskCommand`/watch cancellation、`finalize_task_state`、`mark_task_failed_and_cleanup`、`cleanup_runtime` 和最终 snapshot persist。它必须明确：

- failure signal 的类型、唯一 owner、从 reader 到 session 的优先级，以及与用户 cancel/pause、下载成功/失败、progress channel close 的竞争规则；
- reader 发现 failure 后如何停止消费后续事件，session 如何实际请求或等待下载任务取消，而不是仅在 `DownloadTask::run()` 返回后记录错误；
- cleanup、终态状态机、fragment projection、done timeout 和最终 persist 的精确顺序，及 failure signal 是否在这些步骤前/后被消费；
- cancellation/cleanup 测试与既有 S-04 event-ordering、seed、callback、唯一 Tokio worker 保证如何同时成立。

只有该 preflight 的设计和 RED 测试经独立复审通过后，才可新增 dedicated implementation slice 接入 `PlanComplete` / checkpoint `Unsupported`；在此之前它是 blocker，不是凭空 API。

### 5.4 Delete 与 undo

两者均先取得 `TaskStoreExclusive`；reservation scan 发现任意 future/invalid local snapshot 时，在 undo record、repository、tombstone、local file 和 raw snapshot 改变前失败。

**delete（无本地文件）**：在 reserved load 取得 undo material 后，reserved remove 成功才移除 repository 并写入 undo record。remove 失败时 repository/undo/raw snapshot 不变。

**delete（含本地文件）**：在 exclusive admission 内，先完成 reservation preflight、候选路径的全部授权/验证和 undo material capture；之后才实际删除文件，最后 reserved remove snapshot、再移除 repository。多候选文件的逐个删除不能回滚已经删除的 bytes：

- 任一文件删除失败：停止，不 remove snapshot/repository；已删文件清单进入 repair diagnostic。
- 文件均删除但 remove snapshot 失败：repository 保持，返回 repair-required；不得声称 delete 原子或可恢复本地 bytes。

**undo delete**：先验证尚未移除的 undo record、其 caller-provided snapshot 与 reservation；先 reserved strict restore 成功（含新 revision 和 tombstone 移除）才 insert repository，成功后才消费 undo record。future/invalid/restore failure 均不得先插入 repository 或清除 undo record。

### 5.5 Backup import

导入解析必须先使用 raw envelope，禁止 `Backup<TaskSnapshot>`：

```rust
#[derive(Deserialize)]
struct RawBackupEnvelope {
    version: u32,
    config: AppConfig,
    tasks: Vec<Box<serde_json::value::RawValue>>,
}
```

`Cargo.toml` 为 `serde_json` 启用 `raw_value` feature。读取大小限制、backup version、config validation 和 task count 都在无 mutation 阶段完成；每一个 `RawValue` 交由 store 的唯一 classifier/parse API 处理，因此 duplicate `schemaVersion` 不会在 app deserialize 时消失。任一 future/invalid task 在 config、repository、snapshot、tombstone 和临时文件改变前返回 typed error。

随后：

- `overwrite=false`：取得 exclusive admission/reservation 后，在 reservation 内再预检全部 target key 与 conflict；先保存所有新 snapshot，全部成功后才 insert repository。中途普通 I/O failure 时删除本次新建 snapshot 并恢复其原 tombstone state；补偿失败返回 `RepairRequired`。
- `overwrite=true`：raw backup 与 local namespace 都通过 strict preflight 后，保留旧的 parsed snapshot/tombstone material 作为**补偿材料**；在 reservation 内替换 snapshot namespace，snapshot 阶段全部成功后才 durable persist config，最后才替换内存 config、cache 与 repository。snapshot 或 config 普通 I/O failure 时尽力恢复已改 snapshot/tombstone；恢复失败返回 `RepairRequired`。不添加新 staging format、第二个 store 或“全局 transaction”说法。
- config/repository mutation 必须在可能返回 `Unsupported`/`InvalidData` 的 validation 后；所有 store save/remove 均 await，禁止 detached `spawn_blocking`。

导出在 `TaskStoreShared` 内完成 all-task classification 后才创建 `.tmp`；import 不创建写入临时文件。future local 或 future backup item 均不可被 warning、冲突跳过、overwrite 或 fallback 静默处理。

## 6. 补偿的真实边界

1. **Unsupported/InvalidData 是 strict pre-side-effect failure。** 只要 operation 尚可先检查 future/invalid（reservation scan、single raw guard、incoming snapshot validation、raw backup parse），必须在 config/repository/local-file/snapshot/tombstone 改变前返回；这条路径不进入补偿。
2. **普通 I/O 不是事务。** durable write、rename、directory fsync、delete 或 config persist 可在部分副作用后失败。调用方必须保存最小补偿材料，按相反方向尽力恢复；主操作和补偿均失败时返回含两者原因的 `RepairRequired`，并保留可诊断日志。
3. **snapshot namespace 补偿是语义恢复，不承诺恢复原始 bytes/revision。** 原 snapshot 经 strict restore 可能取得更高 revision；这是 revision/tombstone 不变量优先于字节级回滚的明确取舍。
4. **本地多文件删除不可回滚。** 绝不声称 delete/undo/import 是跨文件 transaction；错误必须告诉调用方 repair required，且 repository/snapshot 保持在仍可诊断的状态。
5. **进程崩溃不在本范围。** tombstone 与 reservation 在进程退出后消失；本设计只要求 strict durable snapshot write 的 I/O error 可观察，不创造 WAL、事务日志或 crash recovery protocol。

## 7. 严格 TDD 切片

每一切片固定为独立 Tester 写可编译的行为 RED → 运行并记录 RED → Coder 只改 production → Tester 重跑 GREEN → 独立 Reviewer 先审规格、再审代码。Tester 与 Coder 不得是同一 Agent。

### S-02a — 纯 store streaming classifier、`RecoveryResult` 与 incoming guard

- **范围与所有权**：仅 `tachyon-store`。本切片只建立 classifier、`ProtectedSnapshot` / `RecoveryResult`、可区分 store error 和 incoming save/restore guard；不触及 `TaskStore`、`AppError`、startup 或任何 app typed outcome。
- **Tester 文件**：`crates/tachyon-store/src/recovery.rs` 内联 tests。
- **Coder 文件**：`crates/tachyon-store/src/recovery.rs`、`crates/tachyon-store/src/lib.rs`。
- **RED 用例**：non-object、重复 `schemaVersion`、null/string/negative/overflow 均 `InvalidData`；legacy 缺字段仍 fallback；future 不进入 `TaskRecord` fallback，出现在 `unsupported_schema` 而非 `corrupt_keys`；future incoming `TaskSnapshot` save/restore 不改 raw/tombstone。
- **GREEN 命令（仅 store）**：
  ```bash
  cargo nextest run -p tachyon-store --lib -- recovery::tests::<exact_test> --exact
  cargo nextest run -p tachyon-store --lib
  ```

### S-02a2 — `TaskStore` / `AppError` / startup typed outcome 传播

- **前置条件**：S-02a 已 GREEN；该切片不得扩大到 admission gate（依赖 S-02b）、runtime、lifecycle mutation 或 delete/import。
- **Tester 文件**：`crates/tachyon-app/src/task_store.rs` tests；`crates/tachyon-app/src/commands/mod.rs` startup tests。
- **Coder 文件**：`crates/tachyon-app/src/task_store.rs`、`crates/tachyon-app/src/commands/mod.rs`。
- **RED 用例**：store `Unsupported` 映射为 `UpgradeRequired`，`InvalidData` 与普通 I/O 保持可区分；startup 合法任务继续恢复，而 future 产生显式 upgrade notice，不能混入 corrupt 或静默丢弃；`TaskStore` public production entry 无 direct manager bypass，且所有相关 tests 经 facade 进入。
- **GREEN 命令（实际 app）**：
  ```bash
  cargo nextest run -p tachyon-app --lib -- task_store::tests::<exact_test> --exact
  cargo nextest run -p tachyon-app --lib -- commands::tests::<exact_test> --exact
  cargo nextest run -p tachyon-app --lib
  ```

### S-02b — reservation capability、普通 API 禁入、restore durable 顺序

- **Tester 文件**：`crates/tachyon-store/src/recovery.rs`、`crates/tachyon-store/src/store.rs` 内联 tests。
- **Coder 文件**：`crates/tachyon-store/src/recovery.rs`、`crates/tachyon-store/src/store.rs`、`crates/tachyon-store/src/lib.rs`。
- **RED 用例**：reservation scan 一见 future/invalid 即不发 token；active token 时普通 load/save/update/remove/restore/batch API 均 `ReservationActive`；伪造 nonce/manager identity 与过期 token 均失败；drop 恰好释放匹配 token；reserved operation 线性化；restore write/directory fsync failure 后 tombstone 保持，旧 revision save 仍被拒；restore success revision 为 `max(tombstone,disk)+1` 后才移 tombstone。
- **GREEN 命令**：
  ```bash
  cargo nextest run -p tachyon-store --lib -- recovery::tests::<exact_test> --exact
  cargo nextest run -p tachyon-store --lib -- store::tests::<exact_test> --exact
  cargo nextest run -p tachyon-store
  ```

### S-02c — facade admission gate 与 fragment view

- **Tester 文件**：`crates/tachyon-app/src/task_store.rs`、`crates/tachyon-app/src/commands/fragment_commands.rs` tests。
- **Coder 文件**：`crates/tachyon-app/src/task_store.rs`、`crates/tachyon-app/src/commands/fragment_commands.rs`。
- **RED 用例**：shared operation 在 exclusive admission 前完成；exclusive 排队后新 shared 不越过；future terminal fragment query 返回 upgrade-required，不是 empty view；所有 `TaskStore` public production wrapper 都需 admission，无法 direct manager bypass。startup outcome 属 S-02a2，不在此切片重复实现。
- **Reviewer 额外检查**：锁顺序、no re-entry、无 guard 跨 await；以 scoped grep 证明 wrapper/read path 不含 `.ok().flatten()` 吞 `Unsupported`。
- **GREEN 命令**：
  ```bash
  cargo nextest run -p tachyon-app --lib -- task_store::tests::<exact_test> --exact
  cargo nextest run -p tachyon-app --lib -- commands::fragment_commands::tests::<exact_test> --exact
  cargo nextest run -p tachyon-app --lib
  ```

### S-02d — ordinary lifecycle mutation 的严格保存 / repository 补偿

- **Tester 文件**：`crates/tachyon-app/src/service/task_service/tests.rs`、`crates/tachyon-app/src/commands/task_commands.rs` tests；create 的 hub/sniffer 入口覆盖按实际所有权加入。
- **Coder 文件**：`crates/tachyon-app/src/service/task_service.rs`、`crates/tachyon-app/src/commands/task_commands.rs`，以及实际持有 create 入口的 `hub_commands.rs` / `sniffer_commands.rs`（仅签名传播需要时）。
- **RED 用例**：`TaskService::create_task`、pause/resume/cancel、tag、order、`persist_task_state`、`persist_task_snapshot` 的 `Unsupported`/`InvalidData` 均在 repository/config/可见任务状态/raw snapshot 变更前失败，或 repository 已被验证补偿；普通 I/O 的未完成补偿为 `RepairRequired`；所有 save 均 await、无 detached/warning 成功。
- **GREEN 命令**：
  ```bash
  cargo nextest run -p tachyon-app --lib -- service::task_service::tests::<exact_test> --exact
  cargo nextest run -p tachyon-app --lib -- commands::task_commands::tests::<exact_test> --exact
  cargo nextest run -p tachyon-app --lib
  ```

### S-02d1 — S-04/S-02 runtime cancellation 与 session failure ownership 设计 preflight

- **性质**：blocking design slice，不实施 `PlanComplete` / checkpoint `Unsupported` 传播。
- **read-set 与产物**：完成 §5.3 所列实际 runtime read-set；写出 failure signal、priority、实际取消、cleanup/终态顺序和测试策略，并由独立 Reviewer 批准。
- **停止条件**：若不能在既有 `DownloadTask::run`、控制信号和 cleanup owner 中证明实际取消路径，停止；不得以改 oneshot payload 取代设计。

### S-02d2 — download lifecycle（不含 PlanComplete/checkpoint runtime failure）

- **前置条件**：S-02d 的 ordinary lifecycle 合同已 GREEN。
- **Tester/Coder 文件**：`crates/tachyon-app/src/commands/task_commands.rs`、`crates/tachyon-app/src/runtime/download_session.rs` 的实际所有权 sections。
- **RED 用例**：future snapshot 使 `inject_resume_snapshot` 在 probe 前停止；`probe_and_save_metadata` future strict save 不改 repository metadata；`None`、普通 corrupt/I/O 的既定非-future 行为仅在明确分支保留。
- **GREEN 命令**：
  ```bash
  cargo nextest run -p tachyon-app --lib -- commands::task_commands::tests::<exact_test> --exact
  cargo nextest run -p tachyon-app --lib
  ```

### S-02d3 — PlanComplete/checkpoint typed propagation（仅在 S-02d1 批准后）

- **前置条件**：S-02d1 的 failure ownership 设计、RED 测试和独立复审均已通过。
- **Tester/Coder 文件**：`crates/tachyon-app/src/runtime/chunk_reader_pool.rs`、`crates/tachyon-app/src/runtime/download_session.rs`、`crates/tachyon-app/src/commands/task_commands.rs` 的实际所有权 sections。
- **RED 用例**：PlanComplete future 不采用 repository seed、不 callback 后续 Chunk；每种 checkpoint 的 future error 按获批的 signal/priority 实际停止 reader 与下载 session，并按获批顺序 cleanup；覆盖它与 cancel/pause、下载完成/失败及 done timeout 的竞争。
- **S-04 边界**：保留 `load_plan_snapshot` 的 `spawn_blocking` hook、oneshot 栅栏、唯一 Tokio worker 和 event ordering 测试；不得把 S-04 当前 `Ok(Err(e))` fallback 改写为已支持 `Unsupported` 的历史叙述。
- **GREEN 命令**：
  ```bash
  cargo nextest run -p tachyon-app --lib -- runtime::chunk_reader_pool::tests::<exact_test> --exact
  cargo nextest run -p tachyon-app --lib -- runtime::download_session::tests::<exact_test> --exact
  cargo nextest run -p tachyon-app --lib
  ```

### S-02e — delete/undo exclusive operation 与不可回滚本地文件边界

- **Tester 文件**：`crates/tachyon-app/src/service/task_service/tests.rs`。
- **Coder 文件**：`crates/tachyon-app/src/service/task_service.rs`，以及为调用签名所需的 `crates/tachyon-app/src/commands/task_commands.rs`。
- **RED 用例**：local future 阻止 delete/undo，断言 repository/raw snapshot/tombstone/local candidate/undo record 不变；incoming future undo snapshot 在 repository insert 前被拒；restore I/O failure 不消费 undo；无本地文件 delete 的 remove failure 不移 repository；多候选文件第二次删除失败保留 snapshot/repository 且返回 repair-required；文件删完但 snapshot remove 失败返回 repair-required，绝不宣称已回滚 bytes。
- **GREEN 命令**：
  ```bash
  cargo nextest run -p tachyon-app --lib -- service::task_service::tests::<exact_test> --exact
  cargo nextest run -p tachyon-app
  ```

### S-02f — RawValue backup、export/import preflight 与补偿

- **Tester 文件**：`crates/tachyon-app/src/commands/task_commands.rs` tests；必要的 manifest test setup。
- **Coder 文件**：根 `Cargo.toml`（启用 `serde_json` `raw_value`）、生成的 `Cargo.lock`、`crates/tachyon-app/src/commands/task_commands.rs`、`crates/tachyon-app/src/task_store.rs`。
- **RED 用例**：backup task 有 duplicate/future `schemaVersion` 时，在 config/repository/raw local snapshot/tombstone/temporary backup 均改变前失败；export local future 时不创建 tmp/target；overwrite/non-overwrite local future 都在 mutation 前失败；non-overwrite 中途 save I/O failure 清理已新建 snapshot，清理失败为 `RepairRequired`；overwrite snapshot/config I/O failure 执行记录的补偿，补偿失败为 composite repair-required；正常 legacy/current backup 回归。
- **GREEN 命令**：
  ```bash
  cargo nextest run -p tachyon-app --lib -- commands::task_commands::tests::<exact_test> --exact
  cargo nextest run -p tachyon-app
  ```

## 8. 精确文件图与退役

| 文件 | S-02 责任 |
|---|---|
| `Cargo.toml`、`Cargo.lock` | 为 `serde_json::value::RawValue` 启用 `raw_value`；lockfile 为生成物。 |
| `crates/tachyon-store/src/recovery.rs` | classifier、typed store error、`RecoveryResult`、reservation、reserved mutation、incoming guard、restore revision/tombstone 顺序。 |
| `crates/tachyon-store/src/store.rs` | strict durable directory fsync error 传播。 |
| `crates/tachyon-store/src/lib.rs` | 仅重导出 S-02 所需 store 类型。 |
| `crates/tachyon-app/src/task_store.rs` | S-02a2 的错误映射/startup facade；S-02c 的 admission gate、shared/exclusive access object、无 bypass wrapper。 |
| `crates/tachyon-app/src/commands/mod.rs` | S-02a2 的 `AppError` typed outcome 与 startup consumer；后续 slice 的命令级传播所有权。 |
| `crates/tachyon-app/src/commands/fragment_commands.rs` | terminal snapshot query 的 typed future failure。 |
| `crates/tachyon-app/src/commands/task_commands.rs` | `TaskService::create_task` command 入口、inject/probe、backup RawValue export/import、command signature propagation。 |
| `crates/tachyon-app/src/commands/hub_commands.rs`、`crates/tachyon-app/src/commands/sniffer_commands.rs` | `create_task_inner` 的 hub/sniffer 入口；仅在 create 的严格保存合同要求签名/错误传播时修改。 |
| `crates/tachyon-app/src/runtime/download_session.rs` | lifecycle typed failure 与 chunk reader result 消费。 |
| `crates/tachyon-app/src/runtime/chunk_reader_pool.rs` | PlanComplete/checkpoint typed outcome；保留 S-04 blocking/order contract。 |
| `crates/tachyon-app/src/service/task_service.rs` | `persist_task_state`、delete/undo 顺序与补偿。 |
| `crates/tachyon-app/src/service/task_service/tests.rs` | delete/undo future 与 repair-required regression assets。 |

**Repair track**：唯一 streaming classifier + store reservation 取代 future-as-corrupt、scan+epoch、restore 先清 tombstone、detached save 与 warning-only destructive workflow。

**Retirement track**：删除所有 production `load_snapshot(...).ok().flatten()`、`spawn_blocking(...save...).detach`、future warning fallback、app 自行 namespace scan 与普通 API bypass；保留 corrupt 的既有隔离语义，但不得接收 future。

**停止条件**：若需要第二个 schema owner、raw JSON app parser、持久化 transaction/WAL、新 snapshot format、跨层依赖，或无法证明 reservation + admission + compensation 的顺序，停止实施并回到设计审查。

## 9. 完整验证

每一严格 TDD 切片 Green 后，最终执行：

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo nextest run -p tachyon-store
cargo nextest run -p tachyon-app
cargo nextest run --all
bash scripts/ci/coverage.sh
```

Reviewer 还必须检查：

1. 所列真实路径与 `TaskStore` wrappers 没有把 `Unsupported` 转为 `None`、corrupt、warning 或 repository fallback；
2. active reservation 下普通 API 一律拒绝，reserved API 均验证 manager identity + nonce；
3. 无 `std::sync::MutexGuard` 跨 await，gate/reservation/repository/config 的锁顺序一致；
4. restore 的 directory fsync error 可观察，tombstone 仅在 strict write 成功后清除；
5. future 负向断言覆盖 raw bytes、repository、config、tombstone、临时 backup 与本地文件；
6. 所有 I/O 补偿失败显式为 `RepairRequired`，文档与代码均不宣称崩溃事务或本地文件回滚。
