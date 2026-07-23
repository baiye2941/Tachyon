# Task intent：S-02 future schema 全失败关闭

## Requested outcome

依据 `C:/Users/白夜/.config/aegis/worktrees/Tachyon/audit-verify-20260722/docs/aegis/specs/2026-07-22-s02-full-fail-closed-followup.md`，以严格 TDD 建立 future schema 的 streaming 分类、受保护结果类别及后续 fail-closed 工作流。

## 当前执行边界

- 仅开始 S-02a 的第一个原子微切片：future raw snapshot 在 batch recovery 中进入 `unsupported_schema`，不得进入 `tasks` 或 `corrupt_keys`。
- 仅 `tachyon-store`；不实施 App error mapping、startup、reservation、admission gate、delete/undo/import、runtime failure propagation。
- 不修改快照格式、不增加 app 层 schema parser、不触及用户真实配置或数据目录。

## TDD Route

```text
TDD Route:
- Mode: auto
- Decision: strict
- Reason: persistence/schema 兼容边界与数据保护行为。
- Verification: 独立 Tester RED → Coder GREEN → 独立 Tester regression → 规格审查 → 质量审查。
```

## Change Necessity

- User-visible need：旧客户端不能将较新版本任务快照当作可读写数据或损坏数据处理。
- No-change / non-code option：文档、warning 或 App 侧 scan 无法阻止 store fallback/重写路径。
- Why code change is necessary：当前 `TaskSnapshot` 后的 `TaskRecord` fallback 没有 streaming header guard，future schema 可进入正常恢复。
- Minimum change boundary：`tachyon-store` 的唯一 header classifier、`RecoveryResult` result category 和对应 batch recovery path。
- Decision：code-change。

## Complexity Budget

- Artifact class：持久化恢复 canonical owner。
- Current pressure：`recovery.rs` 聚合 load/save/update/recovery。
- Projected post-change pressure：一个 header classifier、一个受保护结果类别；不复制 batch loops。
- Planned governance：不创建第二 recovery manager / schema adapter；future 不落入 corrupt。

## Baseline usage

- Required refs：`AGENTS.md`、`.claude/rules/multi-agent-engineering.md`、`docs/architecture.md` §4.9/§4.10、主审计设计 §4、S-02 follow-up §2/§7、实施计划。
- Acknowledged refs：协调者已读；工作树从 `58bc939` 创建且 source clean。
- Missing refs：无；S-02a 不以 app/startup 执行授权为前置。
- ArchitectureReviewRequired：yes。
