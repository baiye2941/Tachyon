# Spec Brief：A-08 BtSession 重建条件 + C2 ci-pass 纳入 MSRV

## 范围
1. **A-08**：`update_config` 在 `magnet` **或** `download_dir` 变化时重建 `BtSession`；先 `BtSession::new` 成功再提交配置与 session 句柄；重建失败则**不**写入新配置并返回错误。
2. **C2**：`.github/workflows/ci.yml` 的 `ci-pass.needs` 加入 `msrv`。

## 非目标
- 已运行中 magnet 任务的跨 session 迁移
- Authenticode / openat2 / fuzz 硬门禁
- ConnectionPool 语义以外的 runtime generation 统一模型

## TDD
strict（app 侧：仅改 download_dir 时标记需重建；重建失败不污染 config）
