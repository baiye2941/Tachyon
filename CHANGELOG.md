# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Security

- **H-3**: DHT 签名从 BLAKE3 对称 MAC 升级为 Ed25519 非对称签名 — 旧方案使用公开的 NodeId 作为 BLAKE3 key，任何知道节点 ID 的网络参与者均可伪造消息；新方案每个节点持有 Ed25519 密钥对，NodeId 从公钥派生，签名只能由私钥持有者生成，防止路由表投毒和 Eclipse 攻击 (`dht/node.rs`, `dht/message.rs`, `dht/transport.rs`)
  - 新增 `NodeIdentity` 结构体（Ed25519 密钥对 + 派生 NodeId）
  - 新增 `DhtSignature` 结构体（64 字节签名 + 32 字节公钥）
  - `KademliaMessage.signature` 从 `Option<[u8; 32]>` 变更为 `Option<DhtSignature>`
  - `DhtTransport::bind()` 新增 `identity: NodeIdentity` 参数

### Added

- **M-14**: 每源熔断器(Circuit Breaker)机制 — 连续失败 5 次后熔断源 30 秒，半开状态允许试探请求，防止持续失败的源浪费连接资源 (`circuit_breaker.rs`, `downloader.rs`)
- **M-24**: CPU blake3 流式哈希 API — 新增 `CpuVerifier::compute_hash_streaming()` 和 `compute_hash_from_path()` 方法，使用 `blake3::Hasher` 的 `update()`+`finalize()` 模式逐块读取，避免 50GB 模型文件等大文件场景下的 OOM (`cpu.rs`)
- **L-27**: 前端依赖安全审计 CI 步骤 — 在 CI workflow 中添加 `npx npm audit --audit-level=moderate` 扫描前端依赖已知漏洞 (`ci.yml`)

### Fixed

- **M-7**: `DownloadError::Other` 变体不再默认可重试 — `From<String>` 隐式转换曾导致配置错误等不可重试情况被无限重试 (`error.rs`)
- **M-8**: `DownloadTask` 的 `id`/`url`/`config` 字段从 `pub` 改为私有，添加只读访问器防止外部修改任务标识 (`downloader.rs`)
- **M-9**: `AppState` 全部 11 个字段从 `pub` 改为 `pub(crate)`，防止外部代码直接操纵内部状态 (`commands/mod.rs`)
- **M-6**: `WritePipeline::write_batch` 信号量许可数 clamp 到容量上限，防止 `acquire_many` 永久阻塞 (`pipeline.rs`)
- **M-15**: `Metrics` 集成到 `DownloadTask` 执行路径 — 下载字节数、分片完成数、错误数等指标现在由生产代码记录，移除 `#[allow(dead_code)]` (`lib.rs`, `downloader.rs`)
- **M-25**: `generate_node_id` 改用 OS CSPRNG (`getrandom`) 替代 SipHash，提升 DHT NodeId 不可预测性 (`dht/node.rs`)
- **H-9**: `handle_message` 中移除冗余 `bucket.contains()` 调用，减少每次消息处理的 k-bucket 线性扫描次数 (`dht/kademlia.rs`)
- **L-9**: `verify()` 中硬编码的 1 MiB 分块大小提取为命名常量 `VERIFY_HASH_CHUNK_SIZE`，提升可读性 (`downloader.rs`)
- **L-12**: 进度上报频率 magic number `5` 提取为命名常量 `PROGRESS_REPORT_CHUNK_INTERVAL`，附选值依据注释 (`downloader.rs`)
- **L-13**: `try_send` 进度事件丢弃时添加 `warn!` 日志，增强可观测性 (`downloader.rs`)
- **L-14**: `apply_terminal_error` 改用 `DownloadState::try_transition()` 优先，非标准路径强制回退 + `warn!` 日志，消除状态机绕过 (`downloader.rs`)
- **L-15**: `token.rs` 测试中 7 处 `unsafe { env::set_var/remove_var }` 添加 Safety 注释，符合项目 CLAUDE.md 强制规则 (`hub/token.rs`)
- **L-16**: `MockProtocol::failing()` 保留原始 `DownloadError` 变体类型（含 `ChecksumMismatch` 的 `expected/actual` 字段），替代旧方案将错误转为字符串再包装为 `Network` 变体 (`test_harness.rs`)
- **L-17**: 多分片并发写入集成测试改为真正的 `tokio::spawn` 并发执行，替代旧方案顺序循环 (`integration_test.rs`)
- **L-21**: `tsconfig.json` 完整启用 `noUncheckedIndexedAccess` — 7 个源文件 + 4 个测试文件中的数组索引访问改用非空断言 `!` 或可选链 `?.`，无 `// @ts-ignore` 绕过 (`frontend/`)
- **L-24**: 移除 `tachyon-app` 的 `staticlib` crate-type，减少编译时间和产物体积 (`Cargo.toml`)
- **L-29**: GPU BLAKE3 缓冲区池化复用 — `GpuVerifier` 缓存 input/output/staging buffer 并按需扩容，避免每次 `compute_blake3` 创建新 buffer；附带修复 4 个 clippy 警告 (`gpu.rs`)
- **L-30**: `find_closest` 算法从 O(n log n) 全量排序优化为 O(n) `select_nth_unstable_by_key` 部分排序 + `HashSet` O(1) 去重 (`dht/kbucket.rs`)
- **M-22**: `deny.toml` 添加 RUSTSEC-2024-0429 (glib unsound) ignore 条目，标注等待 Tauri v3 迁移后移除 (`deny.toml`)

### Changed

- **L-11**: `DownloadConfig::validate()` / `ConnectionConfig::validate()` / `AppConfig::validate()` 中 8 个硬编码校验阈值提取为命名常量（`MAX_CONCURRENT_FRAGMENTS_LIMIT=256` 等），错误消息通过 `format!` 内插常量值 (`config.rs`)
