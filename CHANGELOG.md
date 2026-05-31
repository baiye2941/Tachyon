# Changelog

## [0.2.0] — 2026-06

### 新增

- **限速控制**: `DownloadConfig` 新增 `rate_limit_bytes_per_sec` 字段，分片下载支持全局速度限制
- **HTTP/2 激活**: `HttpClient` 新增 `with_connection_config()` 方法，reqwest 添加 `http2` feature，自适应窗口调优
- **流式哈希校验**: `verify()` 改用 `blake3::Hasher::update()` 增量式校验，峰值内存 O(chunk) 而非 O(fragment)
- **io_uring 写入路径**: `IoUringStorage::submit_write()` 实现 SQE 构造 + CQE 等待，Linux 零拷贝管线就绪
- **Linux fallocate**: `TokioFile::allocate()` 在 Linux 上使用 `fallocate` 预分配真实磁盘块，防止 ENOSPC
- **CI cargo-audit**: `.github/workflows/ci.yml` 新增 `cargo-audit` job，自动扫描依赖漏洞
- **amd-hub crate**: 新增 HuggingFace Hub API 客户端，支持文件树列表、LFS 指针解析、Token 管理、镜像 endpoint
- **P2SP 多源镜像**: `MirrorProtocol` + `with_mirrors()` 构造方法，主源失败自动 fallback 到备用 URL
- **DownloadSource::url()**: P2SP CDN 源暴露下载 URL

### 修复

- `FileStore::set` 改为 write-to-temp-then-rename 原子写入（崩溃一致性）
- `SchedulerConfig` 新增 `default_target_fragments` 字段（语义修正: 分片目标数 ≠ 连接池连接数）
- `plan_fragments` 使用 `default_target_fragments` 替代 `PoolConfig::max_global`
- io_uring `offset` 类型修正 (`i64` → `u64`)、`sq.push` 引用传递
- `IoUringHandle._ring` 改为 `Mutex<IoUring>` 支持内部可变性
- `AmdError::Http` 字段名修正 (`reason`)

### 变更统计

- 新增 crate: `amd-hub` (HF Hub API 客户端)
- 新增组件: `MirrorProtocol` (多源 fallback 适配器)
- 8 个文件变更, 2 个新文件
- 11 个新测试 (amd-hub)
- 零 clippy 警告, 零 unsafe (非测试)

---

## [0.1.0] — 2026-05

### 初始版本

- 10 crate workspace: amd-core, amd-engine, amd-scheduler, amd-io, amd-protocol, amd-crypto, amd-p2sp, amd-sniffer, amd-store, amd-app
- 多线程分片下载 (Holt-Winters 自适应调度)
- HTTP/HTTPS/QUIC/FTP 多协议支持
- 断点续传 (TaskSnapshot + RecoveryManager)
- 浏览器资源嗅探 (Playwright MCP)
- Tauri v2 + Solid.js 前端
- 604+ 测试, 零 clippy, 零 unsafe
