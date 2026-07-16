# Spec Brief：Download 代理/ioStrategy UI + probe 配置同源

## 范围
1. 前端 `ConfigDraft` / `DownloadTab` 暴露 `download.proxy` 与 `download.ioStrategy`，`buildPatch`/`applyConfig` 往返。
2. i18n 中英文文案；proxy 提示 SSRF 信任边界（与 SEC-007 一致）。
3. **A-06 部分**：`probe_filename` HTTP 路径使用 `build_download_config(&app_config, …)` 而非 `DownloadConfig::default()`，使 proxy/UA/timeouts/io_strategy 与正式任务同源（仍不注入全局限速器/连接池，probe 为轻量 HEAD）。

## 非目标
- 完整 `DownloadSource` 统一路由工厂（A-06 全量）
- A-07 Sniffer ResourceManager 合并
- openat2 / Authenticode

## TDD
strict（后端 probe 配置同源单测 + 前端类型/patch 字段）
