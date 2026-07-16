# Spec Brief：A-06 DownloadSource 统一解析

## 范围
1. 在 `tachyon-core` 定义 `DownloadSourceKind` + `DownloadSource`（规范化 URL 字符串 + kind）。
2. 单一 `parse_download_source`：magnet / HLS(.m3u8|.m3u path) / HTTP(S)，拒绝其他 scheme。
3. 接线：
   - app `validate_download_url` 经 `parse_download_source`（仍做 magnet URI 细节校验与 HTTP SSRF）
   - `build_download_task` 用 `source.kind` 而非裸 `starts_with("magnet:?")`
   - engine `looks_like_hls_url` 与 magnet 前缀判断委托 core
   - `probe_filename` magnet 分支用 `source.kind == Magnet`

## 非目标
- 完整 engine factory 大重构 / 消除所有字符串前缀
- A-01 app 跨层依赖清理
- A-02 真 TCP 连接池

## TDD
strict
