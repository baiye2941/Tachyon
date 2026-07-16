# Spec Brief：HTTP-10 QUIC 诚实化 · BT-11 高隐私模式

## 范围
1. **HTTP-10**：`ConnectionConfig.enable_quic` 默认改为 `false`；文档说明真 H3 依赖 `http3`+`reqwest_unstable`；导出 `effective_quic_enabled(want)`；app 配置快照暴露 `http3Compiled`
2. **BT-11（最小）**：`MagnetConfig.high_privacy`（默认 false）。启用时：强制 `disable_dht`、不注入全局/session 公共 tracker、`enable_upnp=false`。文档诚实：librqbit metadata resolve 仍可能在 private 标志未知前探索，本开关是应用层最大程度隔离

## 非目标
- 真 H3 prior_knowledge 产品启用
- 上游 librqbit private-first metadata 隔离
- 前端大改 UI 文案（最小：若有字段则接 http3Compiled）

## TDD
strict
