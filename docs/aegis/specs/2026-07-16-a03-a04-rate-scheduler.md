# Spec Brief：A-03 全局 RateLimiter · A-04 SchedulerConfig 生产接线

## 范围
1. **A-03**：`InfraState` 持有唯一 `Arc<RateLimiter>`；`build_download_task` 对所有任务 `set_rate_limiter`；`update_config` 时 `update_rate`（None→0）。
2. **A-04**：`build_download_task` 使用 `AppConfig.scheduler` 构造 `AdaptiveDownloadScheduler::new`，并对 `DownloadTask` `set_scheduler_config`，禁止 `default_config()` 忽略 UI 配置。
3. 测试：全局限速句柄更新；非默认 scheduler 注入路径（构造不 panic + set_scheduler_config 被调用可观测字段若可及）。

## 非目标
- 完整 DownloadRuntime factory 大重构
- 已在跑任务的 scheduler 热替换
- A-09 前端代理可见性完整 UI

## TDD
strict
