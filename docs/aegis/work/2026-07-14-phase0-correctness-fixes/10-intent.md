# Phase 0 正确性修复 — 意图

- 日期：2026-07-14
- 基线起点：`5dd8bc7c37e0440c6ccc85aae8724ab9c6751a62`
- 来源：`Document/PI/Tachyon-Deep-Audit-2026-07-14/Tachyon-系统性深度审查报告-2026-07-14.md` Phase 0
- 请求结果：在 TDD + 多 Agent 交叉验证下，清除静默损坏/UB/错误完成的最高优先级阻断项
- 方法约束：严格 TDD（RED→GREEN→REFACTOR）；设计批准前不写生产代码；修复后多 Agent 独立复核

## TaskIntentDraft

- **Outcome**：Phase 0 阻断项以可证伪测试锁定并修复，默认路径不再引入新静默损坏
- **Goal**：按批准范围修复审计 P0 中“可本地证明”的正确性缺陷
- **Success evidence**：
  - 每个修复有先失败后通过的定向测试
  - `cargo nextest` 相关包通过；涉及 unsafe/FFI 有 Safety 注释
  - clippy `-D warnings` 对改动 crate 通过
  - 交叉验证 Agent 确认无回归到“假绿/假修复”
- **Stop condition**：范围外的 BT 真实 swarm、公网、HLS 产品接线、work-stealing 性能收益不在本切片
- **Non-goals**：性能优化、竞品对照实现、发布签名、前端大改
- **Scope（待用户确认）**：见下一检查点的选项
- **Risks**：超大 `downloader.rs` 改动冲突；Windows/Unix 分叉；假测试只测注释不测行为

## BaselineReadSetHint

- 主报告 Phase 0
- cross-reviews/01-memory-io-p0.md
- cross-reviews/02-concurrency-workstealing.md
- cross-reviews/03-http-hls.md
- AGENTS.md / performance-engineering / multi-agent-engineering

## BaselineUsageDraft

- Required：主报告 P0-1..P0-5；cross-review 01/02/03
- Decision：`needs-user-scope-decision`

## ImpactStatementDraft

- Layers：io / engine / core config / 可能 app checkpoint
- Owners：`AlignedBuf`、`available_disk_space_inner`、`download_single_fragment` retry、work-stealing 退出条件
- Invariants：无静默丢字节；无 FFI 短缓冲；默认 work-stealing 仍关闭或启用后不错误完成
- Compat：API 尽量不破；unsafe 契约可收紧
