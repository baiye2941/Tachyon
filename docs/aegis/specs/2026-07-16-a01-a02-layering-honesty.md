# Spec Brief：A-01 层序收敛 + A-02 命名/文档诚实

## 范围
1. **A-01**：engine 再导出 `BufferPool`、`AdaptiveDownloadScheduler` 工厂与 `sha256_verifier`；app 不再直接依赖 `tachyon-io` / `tachyon-scheduler` / `tachyon-crypto`；`build_download_task` 用 engine 工厂。
2. **A-02**：`ConnectionPool` 增加 `ConcurrencyLimiter` 类型别名与 `active_requests` 指标别名；修正 architecture / user-guide / README 中“TCP 连接池复用”误述，明确为并发许可器 + reqwest Client 真连接池（HTTP-15 registry）。

## 非目标
- 完整 DownloadRuntime 大重构
- ConnectionPool 全仓重命名（保留兼容名）
- openat2 / Authenticode / AsyncCancel / A-10 quick-xml 升级 epic

## TDD
strict（app 不再编译直连底层 crate；engine 工厂单测；文档关键词可检索）
