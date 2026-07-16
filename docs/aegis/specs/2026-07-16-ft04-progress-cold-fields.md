# Spec Brief：FT-04 updateProgress cold 字段判定

## 目标

`updateProgress` 写 `tasks` store 的条件除 hot/status/size 外，MUST 纳入：

- `fragmentsTotal` 变化
- `activeConcurrency` 变化
- `errorReason` 变化（含显式 `null` 清空）

## 非目标

- 完整 hot/cold 渲染隔离（TaskItem 只读 `$hotProgress`）—— 可另开 slice
- 后端 ProgressEvent 协议变更

## TDD

- 仅 concurrency 变化 → task 更新
- 仅 fragmentsTotal 0→N → task 更新
- status 已 failed、补发 errorReason → 写入
- errorReason: null → 清空
