# Spec Brief：A-07 Sniffer 唯一 owner

## 范围
1. `tachyon-sniffer::ResourceManager` 成为资源与 `CaptureConfig` 唯一存储 owner。
2. 补齐生产缺口：配置校验（filter 长度/数量/去重）、容量上限、`on_request` 返回新资源、`get_by_id` 已有。
3. `app::SnifferService` 退化为 `Arc<ResourceManager>` 的薄适配层，删除重复 Vec/UUID 存储与重复规则。
4. 手动 URL 无 file_size 时跳过 min_size（与 ResourceManager 既有语义一致）；有 size 时应用 min_size。

## 非目标
- 浏览器 adapter / CDP
- 双写兼容期
- A-06 全量 DownloadSource

## TDD
strict
