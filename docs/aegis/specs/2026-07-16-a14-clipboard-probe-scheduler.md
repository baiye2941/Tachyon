# Spec Brief：A-14 clipboard on-demand + probe 调度同源

## 目标

1. **ClipboardWatcher**：`start()` 始终 spawn 轮询循环（幂等），循环内按 `enable_watch` 门禁；`update_config` 将 false→true 无需重启即可生效。
2. **前端**：GeneralTab 去掉「需重启」误导，改为即时生效说明。
3. **probe_filename**：HTTP 路径使用 `create_adaptive_scheduler(app_config.scheduler)`，不再 `DownloadTask::new` 默认调度器（与 A-04/A-06 配置同源）。

## 非目标

- 完整 ConfigApplier 字段矩阵
- 轮询间隔热改（interval 仍取启动时 max(100, poll_interval_ms)；间隔变更仍可新任务/重启语义）
- A-12 文件拆分

## TDD

- `test_clipboard_start_is_idempotent`（AtomicBool 二次 start 不双 spawn）
- `test_probe_filename_uses_app_scheduler_config` 若难测则 `build` 路径编译 + 调度器 create 门面已有测试
- 既有 `evaluate_clipboard_text` 套件保持绿
