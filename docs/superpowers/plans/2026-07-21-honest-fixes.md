# 诚实性三项修复计划（TDD + 多 Agent）

**Goal:** 消除三项产品不诚实/脚枪：CLI 硬编码代理、续传丢镜像、任务级 retry_count 恒 0。

**Architecture:** 纯函数/快照字段/进度聚合，最小改动，行为可测。

**Tech Stack:** Rust workspace, nextest, serde 快照 schema

## Global Constraints

- TDD：先失败测试，再最小实现；禁止先写生产代码
- 注释/提交中文；标识符英文
- 不降门禁；不扩大 magnet feature 面
- 快照新增字段必须 `#[serde(default)]`，schema_version +1
- 测试确定性、可并行、不依赖 7897 代理
- 不 mock 核心逻辑；允许 tempdir / env

## 测试缝（Seams）

1. **CLI 代理解析** — 纯函数：CLI 参数 > 环境变量 > None（不硬编码）
2. **TaskSnapshot.mirror_urls** — 创建持久化 ↔ 反序列化 ↔ `restart_download` 恢复
3. **TaskInfo.retry_count** — 分片 `mark_failed` 可重试时任务级累计 + 快照往返

---

### Task 1: CLI 代理去硬编码

**Files:**
- Create/Modify: `src/main.rs`（提取可测函数 + 主流程使用）
- Test: 同文件 `#[cfg(test)]` 或 `src/cli_proxy.rs` 模块

**RED:**
- `resolve_socks_proxy(Some("socks5://a:1"), env=None) == Some("socks5://a:1")`
- `resolve_socks_proxy(None, env=Some("socks5://b:2")) == Some("socks5://b:2")`（读 ALL_PROXY/HTTPS_PROXY 约定：参数优先，测试注入 env map）
- `resolve_socks_proxy(None, None) == None`（**不得**返回 7897）

**GREEN:** 主流程用解析结果；默认无代理。

**验收:** `cargo nextest run -p tachyon -- cli_proxy` 或等价包名过滤全绿。

---

### Task 2: 快照持久化 mirror_urls + restart 恢复

**Files:**
- `crates/tachyon-store/src/recovery.rs` — `TaskSnapshot.mirror_urls: Option<Vec<String>>`，`SNAPSHOT_SCHEMA_VERSION` 6→7
- `crates/tachyon-app/src/task_store.rs` — `task_info_to_snapshot` / `snapshot_to_task_info` 若 TaskInfo 也加字段
- `crates/tachyon-app/src/commands/mod.rs` — `TaskInfo` 增加 `mirror_urls`（可选，serde default）
- `crates/tachyon-app/src/service/task_service.rs` — 创建时写入 mirrors
- `crates/tachyon-app/src/commands/task_commands.rs` — `restart_download` 从 TaskInfo/快照取 mirrors

**推荐：** TaskInfo + Snapshot 都存 `mirror_urls`，创建时写入，restart 直接 `task.mirror_urls`。

**RED:**
- store: 带 mirror_urls 的快照 JSON 往返
- app: create 带 mirrors 的任务后快照含 mirrors；`restart_download` 路径传入的 mirrors 非 None（可用 supervisor mock 或记录 start_download 参数的测试钩——优先测 snapshot 字段 + 纯函数 `mirrors_for_restart(task)`）

**GREEN:** 最小接线。

---

### Task 3: 任务级 retry_count 聚合

**Files:**
- engine: 在 `mark_failed` 成功可重试路径发出可观测信号，或在 app progress 路径累计
- 最小方案：`FragmentProgress` 增加 `Retry { fragment_index, attempt }`，downloader 在 `mark_failed` Ok(true) 后 try_send；`ProgressBroker`/`task_fn` 消费后 `task.retry_count += 1` 并可选 persist

**更小方案（优先）：**  
不改 FragmentProgress 枚举：在 app 层提供 `TaskRepository` 更新 API + 从 engine 已有事件不够时，在 downloader 发送 Chunk completed=false 不表达 retry。

**选定方案：** 扩展 `FragmentProgress::Retry { fragment_index, attempt: u32 }`，TDD 先测枚举与 broker 累加，再在 downloader `mark_failed` 后发送。

**RED:**
- broker/repository：收到 Retry 后 TaskInfo.retry_count 递增
- snapshot 往返保留非零 retry_count（已有字段，补非零断言）

**GREEN:** 接线 + 更新 A-13 注释去掉「恒为 0」。

---

### 交叉验证

每任务：implementer → 独立 reviewer（spec + quality）→ 必要 fix → ledger。  
全部完成后 whole-branch review。

## 完成标准

- 三任务 nextest 相关包全绿
- 无硬编码 7897 作为默认生产路径
- 镜像续传可恢复
- retry_count 在分片失败重试时 > 0
- CI 可本地：`cargo nextest run -p tachyon-store -p tachyon-app -p tachyon-core -p tachyon-engine --lib`（按改动裁剪）
