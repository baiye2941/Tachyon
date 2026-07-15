# Checkpoint — Phase 0 连续修复

- Approved spec：`docs/aegis/specs/2026-07-14-phase0-correctness-fixes.md`
- Plan：`docs/aegis/plans/2026-07-14-phase0-correctness-fixes.md`
- 后续切片设计：`docs/aegis/specs/2026-07-14-p0-6-7-8-object-hls-bt-design.md`
- 后续工作树：`docs/aegis/work/2026-07-14-p0-6-7-8-object-hls-bt/`

## 已收口：F1–F6

| 切片 | 状态 |
|---|---|
| F1–F6 | **Approve**（见历史 checkpoint 条目） |

## 已收口：P0-6 / P0-7 / P0-8（审计编号）

对应 plan 中 F7 BT lifecycle + F8 object identity，以及审计 P0-7 HLS 最小接线：

| 切片 | 状态 |
|---|---|
| P0-6 Object identity | **DONE**（If-Range / resume / mirror） |
| P0-8 BT ownership+cancel | **DONE**（open_sync / preferred / cache binding / cleanup / ops_gate） |
| P0-7 HLS 最小 VOD | **DONE**（URL 选择 / ENDLIST 门 / mirrors 拒绝 / e2e） |

证据：`docs/aegis/work/2026-07-14-p0-6-7-8-object-hls-bt/90-evidence.md`

## Residual（诚实）

1. session pause/delete 后台 + 5s 超时；ops_gate 串行化后 add 会等 cleanup
2. preferred=None 时仍写 raw URL 兼容键
3. 工作区混有 F1–F6 与 P0-6/7/8 未提交改动；用户要求时再 commit

## ResumeStateHint

Phase0 正确性主链 F1–F6 + P0-6/7/8 已收口。勿在未做架构评审前重开 work-stealing。下一步仅用户要求：提交 / 新切片。
