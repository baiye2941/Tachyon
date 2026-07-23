# 检查点：S-02 future schema 全失败关闭

## TodoCheckpointDraft

- **Current todo：** S-02a / 微切片 1：独立 Tester 为 `future_schema_is_reported_as_unsupported_not_corrupt` 创建并实际观察 RED。
- **Active slice：** store streaming header classifier/result category。
- **Completed todos：** 创建隔离 worktree `feat/audit-s02-fail-closed`，基线 `58bc93923fe93031330589c96afc22ac55c23a6c`，起始 source 工作树 clean。
- **Explicit non-edits：** 不改 App、TaskStore facade、startup、reservation、restore fsync、delete/import、PlanComplete/runtime；不接触真实用户配置或数据目录。
- **Evidence refs：** `10-intent.md`；S-02 follow-up §2/§7 S-02a；主审计 spec §4；implementation plan Slice 2。
- **Blocked-on：** Quality Clippy 门禁已由独立 Tester 的 test-only等价 `as_deref()` 修复，fresh all-targets clippy 通过。剩余闭环风险是 code review 提出的 public `recover_pending_tasks` 删除兼容性与 `load_all` mixed/invalid mutation coverage；但精确 follow-up 主设计已允许旧 API 重定向或仅向其直接 caller返回 Unsupported，并明确仓内 caller不得依赖。仓内 grep 为零、`tachyon-store` metadata 无 publish registry/repository，因此此工作树的 delete-first retirement 有证据；额外matrix作为下一微切片而非阻塞当前 core typed-error contract。
- **Next step：** 进入独立 final S-02a spec/quality review，检查 delete-first retirement证据与已通过门禁；若发现必须增加 direct `load_all` / invalid mutation test，则新开 Tester RED，不直接改production。

## Slice Card

- **Goal：** future snapshot 受保护、可分类，不进入合法恢复或 corrupt bucket，raw bytes 不变。
- **Parent plan/spec：** `2026-07-22-s02-full-fail-closed-followup.md` §2.1/§2.2/§7 S-02a。
- **Files：** Tester：`crates/tachyon-store/src/recovery.rs` 测试区；Coder：`recovery.rs`、`lib.rs`。
- **Boundary：** one streaming top-level-object classifier；不新增 app schema parser、reservation 或 backup adapter。
- **Verification：** exact nextest RED/GREEN、tachyon-store regression、fmt/clippy；coverage later after full slice.
- **Stop：** 若测试需要先 deserialize `serde_json::Value`、future 进入 legacy fallback、或需要扩展到 App 才能表达当前行为，暂停设计审查。

## DriftCheckDraft

- 原始用户目标（future schema 全失败关闭）仍被当前切片服务：是。
- compatibility boundary（legacy/current schema）保持：是。
- 新 owner/fallback：否；仅扩展现有 RecoveryManager 的结果模型。
- persistent-state risk：当前仅 temp fixture；无 live data mutation。
- Evidence 状态：strong-but-not-final。第二轮 typed-error/fail-closed matrix GREEN、92/92 store tests、all-target clippy、fmt/diff、workspace build均有新鲜证据；等待最终独立spec/quality closure。
- Decision：final-review-pending。

## ResumeStateHint

恢复时先重读本文件、`10-intent.md`、S-02 follow-up §2/§7、独立 review findings，以及 worktree git status。当前 source 同时含未验收 production diff；不得将其当作可接受基线或跳过新 RED。
