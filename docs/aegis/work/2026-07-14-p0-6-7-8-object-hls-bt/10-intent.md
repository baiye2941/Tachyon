# Intent — P0-6/7/8

- Spec: `docs/aegis/specs/2026-07-14-p0-6-7-8-object-hls-bt-design.md`
- Plan: `docs/aegis/plans/2026-07-14-p0-6-7-8-object-hls-bt.md`
- Parent: phase0 F1–F6 已收口

## TaskIntentDraft

- Outcome: 对象身份闭环、HLS VOD 产品可达、BT 所有权+取消正确
- Success: 定向 RED→GREEN 测试；相关 crate nextest/clippy；交叉复核
- Stop: 不做 live HLS、BT 隐私依赖、性能优化
- TDD: strict
- Order: P0-6 → P0-8 → P0-7

## BaselineUsageDraft

- Required: audit P0-6/7/8；cross-reviews/03；agent-reports 03/04/05；phase0 checkpoint
- Decision: continue
