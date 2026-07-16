# Spec Brief：FT-04 hot 读路径 + FT-07 force_mirror + FT-16 chunk 拆分

## 目标

1. **FT-04 渲染隔离(最小)**：`TaskItem` 进度/速度/已下载从 `$hotProgress` 读取，缺失时回退 `props.task`；cold 字段仍读 task。
2. **FT-07 镜像仍写 HfTaskMeta**：`batch_create_hf_tasks` 增加 `force_mirror`；为 true 时 URL 改写为 `hf-mirror.com` resolve 仍注入 `HfTaskMeta`。前端镜像批量/单文件走该参数。
3. **FT-16**：`vite` `manualChunks` 不再把所有懒加载面板压成单一 `panels` chunk，按面板/vendor 拆分。

## 非目标

- 完整列渲染器 hot 订阅重写
- 改变默认 Hub `source_mode` 行为
- Playwright E2E(FT-08)

## TDD

- vitest：TaskItem hot 覆盖 task 旧 progress
- hub_commands 或 app test：force_mirror URL 含 hf-mirror 且 meta 存在
- 前端 mock `batchCreateHfTasks` 带 forceMirror
