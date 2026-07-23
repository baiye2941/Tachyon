# Task intent：BT Session 生命周期收敛

## Requested outcome

按 `C:/Users/白夜/.config/aegis/worktrees/Tachyon/perf-magnet-download/docs/aegis/specs/2026-07-22-magnet-session-lifecycle-design.md` 实施已批准的严格 TDD 工作流，使 BT acquisition、observer、取消与 cleanup 有唯一、可等待、可重试的生命周期 owner。

## 当前执行边界

- 当前仅开始 Slice 3A 的第一个原子微切片：`cleanup_before_acquisition_completion_closes_scope_and_rejects_late_registration`。
- 生产 owner 预期在 `tachyon-protocol` 的新 lifecycle seam；未进入 engine/CLI/App 接缝、真实 loopback observer 或公网测量。
- 不编辑 `Protocol` / `TaskRunner` trait、TaskSnapshot / TaskInfo / Tauri IPC schema、下载分片算法或 Storage 契约。

## TDD Route

```text
TDD Route:
- Mode: auto
- Decision: strict
- Reason: 该切片修改共享协议层资源所有权与取消/cleanup 合同。
- Verification: 独立 Tester RED → Coder GREEN → 独立 Tester 回归 → 规格审查 → 质量审查。
```

## Change Necessity

- User-visible need：取消、probe failure 或完成后不能把未收敛的 librqbit torrent 误报为已清理。
- No-change / non-code option：仅记录日志或保留 detached cleanup 无法让 acquisition drop/abort 路径可追踪收敛。
- Why code change is necessary：当前 cache/URL gate/后台 spawn 不能表示 acquisition provenance 或可等待 cleanup。
- Minimum change boundary：protocol lifecycle seam、窄接入 `magnet.rs` / `lib.rs`；本微切片先只实现 scope-close 与 late-registration 拒绝。
- Decision：code-change。

## Complexity Budget

- Artifact class：共享协议层 lifecycle/state machine。
- Current pressure：`magnet.rs` 约 3911 行，已混合 session、stream 与测试。
- Projected post-change pressure：新增专属 lifecycle owner，避免继续向 `magnet.rs` 塞入状态机。
- Planned governance：每个微切片只增加一个已测试状态转换；不得新增第二 registry、fallback 或全局 actor。

## Baseline usage

- Required refs：`AGENTS.md`、`.claude/rules/multi-agent-engineering.md`、`.claude/rules/performance-engineering.md`、`docs/architecture.md` §4.5/§4.10、父 strict TDD plan、父 checkpoint/evidence、生命周期设计 spec。
- Acknowledged refs：上述文件均已由协调者读取；lifecycle worktree 位于冻结提交 `58bc939` 且开始时 clean。
- Missing refs：无；真实公网吞吐不属于当前微切片。
- ArchitectureReviewRequired：yes。
