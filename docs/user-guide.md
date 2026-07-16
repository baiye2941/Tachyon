# Tachyon 使用指南

本文档面向使用者与贡献者，涵盖功能特性、配置说明、构建运行、已知限制与贡献流程。

## 目录

- [1. 功能特性](#1-功能特性)
- [2. 配置说明](#2-配置说明)
- [3. Feature Flags](#3-feature-flags)
- [4. 环境变量](#4-环境变量)
- [5. 构建与运行](#5-构建与运行)
- [6. 发布构建优化](#6-发布构建优化)
- [7. 已知限制](#7-已知限制)
- [8. 贡献指南](#8-贡献指南)

---

## 1. 功能特性

### 1.1 多协议下载

Tachyon 支持以下传输协议，可通过 Feature Flag 裁剪：

| 协议 | 实现 | 说明 |
|------|------|------|
| HTTP/HTTPS | reqwest + rustls + HTTP/2 | 始终启用，支持 Range 分片与流式下载 |
| HTTP/3 (over QUIC) | reqwest `http3` feature | 启用 `http3` feature 后可用（需 `--cfg reqwest_unstable`，经 Alt-Svc 协商升级） |
| BitTorrent Magnet | librqbit | 启用 `magnet` feature 后可用 |

### 1.2 下载核心能力

- **多线程分片下载**：根据带宽预测动态规划分片大小，使用 `JoinSet` 并发执行。
- **多源竞速**：`MirrorProtocol` 对多个镜像源并行 probe，主源失败自动 fallback。
- **断点续传**：任务快照持久化到 `tachyon-store`，支持分片级与字节级续传。
- **流式哈希校验**：下载过程中增量计算 BLAKE3 / SHA-256，完成后再做完整性校验。
- **限速控制**：无锁令牌桶，**跨任务共享**同一 `RateLimiter`（进程总带宽上限；非每任务各自一套）。
- **浏览器资源嗅探**：通过文件扩展名识别视频、音频、文档、压缩包等资源类型。
- **HuggingFace Hub 集成**：浏览模型仓库、解析 LFS、管理 Token、批量创建下载任务。

### 1.3 零拷贝 I/O

| 平台 | 后端 | 说明 |
|------|------|------|
| Linux 5.4+ | io_uring | O_DIRECT + fixed buffer，绕过页缓存 |
| Windows | IOCP / WinFile | Overlapped I/O + NO_BUFFERING |
| macOS / 其他 | TokioFile | 标准 tokio::fs 异步 I/O 回退 |

---

## 2. 配置说明

所有配置定义于 `tachyon-core::config`，前端类型定义于 `frontend/src/types.ts`。

### 2.1 `DownloadConfig`（下载配置）

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `download_dir` | String | — | 下载目录（须在 `authorized_dirs` 内） |
| `max_concurrent_fragments` | u32 | 8 | 单任务最大并发分片数（上限 256） |
| `max_retries` | u32 | 5 | 分片下载失败最大重试次数（上限 100） |
| `request_timeout_secs` | u64 | 60 | 单次读取空闲超时（上限 3600） |
| `connect_timeout_secs` | u64 | 10 | 连接超时（上限 300） |
| `verify_checksum` | bool | false | 是否启用哈希校验 |
| `verify_strategy` | VerifyStrategy | BestEffort | 校验策略（Require / BestEffort / Skip） |
| `pause_timeout_secs` | u64 | 300 | 暂停最大持续时间（上限 86400） |
| `rate_limit_bytes_per_sec` | Option<u64> | None | 全局限速（None 为不限速） |
| `max_full_stream_bytes` | usize | 64MB | `download_full` 最大允许字节数 |
| `authorized_dirs` | Vec<String> | [download_dir] | 授权写入目录白名单 |
| `io_strategy` | IoStrategy | Windows: Iocp, 其他: Standard | I/O 后端选择 |
| `user_agent` | String | Tachyon 默认 UA | HTTP 请求 User-Agent |
| `headers` | HashMap<String, String> | 空 | 自定义 HTTP 请求头 |

### 2.2 `AppConfig`（应用根配置）

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `max_concurrent_tasks` | u32 | — | 全局最大并发任务数（上限 100） |
| `download` | DownloadConfig | — | 下载配置 |
| `connection` | ConnectionConfig | — | 并发许可/连接相关配置（非 TCP 池本身） |
| `scheduler` | SchedulerConfig | — | 调度器配置 |
| `magnet` | MagnetConfig | — | BitTorrent 配置（`magnet` feature） |

### 2.3 I/O 策略

```rust
pub enum IoStrategy {
    Standard,   // tokio::fs 标准异步 I/O
    WinAligned, // Windows NO_BUFFERING + 对齐写入
    Iocp,       // Windows IOCP
    IoUring,    // Linux io_uring
}
```

- Windows 默认使用 `Iocp`。
- Linux 默认使用 `IoUring`（内核 5.4+）。
- macOS 与其他平台自动回退到 `Standard`。

---

## 3. Feature Flags

| Feature | 默认 | 作用 |
|---------|------|------|
| `magnet` | 启用 | 编译 BitTorrent 磁力链接支持（librqbit） |
| `http3` | 禁用 | 编译 HTTP/3 支持（reqwest `http3` feature，需 `--cfg reqwest_unstable`，经 Alt-Svc 协商升级） |
| `gpu` | 禁用 | GPU 加速哈希校验（wgpu，实验性） |

```bash
# HTTP + Magnet（默认）
cargo build

# 注意:根包当前未定义 feature 开关,`--no-default-features` 不会裁剪磁力支持
# (磁力由 tachyon-protocol 的 default feature 控制)。如需最小 HTTP 构建,
# 需在 tachyon-protocol 层关闭 default-features:
#   cargo build -p tachyon-protocol --no-default-features
cargo build

# 启用 QUIC / HTTP3（reqwest http3 标记为 unstable，须注入 cfg）
RUSTFLAGS='--cfg reqwest_unstable' cargo build --features tachyon-protocol/http3
```

---

## 4. 环境变量

| 变量 | 用途 |
|------|------|
| `HF_TOKEN` | HuggingFace Hub API 访问令牌（tachyon-hub 读取） |
| `RUST_LOG` | tracing 日志级别（默认 info） |

---

## 5. 构建与运行

### 5.1 环境要求

| 依赖 | 最低版本 | 说明 |
|------|----------|------|
| Rust | 1.85 | edition 2024，见 `rust-toolchain.toml` |
| Bun | 最新 | 前端包管理与构建 |
| cargo-tauri | 2.x | Tauri 开发与构建 CLI |

### 5.2 构建命令

```bash
# 克隆
git clone https://github.com/baiye2941/Tachyon.git
cd Tachyon

# 调试构建（默认 HTTP + Magnet）
cargo build

# 发布构建
cargo build --release

# Feature 裁剪(注意:根包未定义 feature,以下需在 tachyon-protocol 层执行)
# cargo build -p tachyon-protocol --no-default-features   # 仅 HTTP(关闭磁力)
# cargo build -p tachyon-protocol --features magnet       # HTTP + Magnet(同默认)
RUSTFLAGS='--cfg reqwest_unstable' cargo build --features tachyon-protocol/http3  # 启用 QUIC/HTTP3
```

### 5.3 开发模式

```bash
# 安装前端依赖并启动 Vite 开发服务器
cd frontend && bun install && bun run dev

# 启动 Tauri 开发模式（同时启动前端 + Rust 后端）
cargo tauri dev
```

### 5.4 测试命令

```bash
# Rust 测试（推荐 nextest）
cargo nextest run --all

# 单 crate
cargo nextest run -p tachyon-core

# clippy 零警告
cargo clippy --all-targets --all-features -- -D warnings

# 格式检查
cargo fmt --all -- --check

# 覆盖率（核心 crate）
cargo llvm-cov -p tachyon-core -p tachyon-engine -p tachyon-store \
  -p tachyon-io -p tachyon-crypto -p tachyon-scheduler \
  --fail-under-lines 90 --summary-only

# 前端测试
cd frontend && bun run test

# 前端 E2E
cd frontend && bun run test:e2e
```

---

## 6. 发布构建优化

根 `Cargo.toml` 中的 `profile.release`：

```toml
[profile.release]
opt-level = 3        # 最高优化级别
lto = true           # 链接时优化
codegen-units = 1    # 单编译单元
strip = true         # 剥离符号表
panic = "abort"      # panic 时直接终止
overflow-checks = false
```

前端构建同样启用压缩与 Tree Shaking：

```bash
cd frontend && bun run build
```

---

## 7. 已知限制

| 限制 | 说明 |
|------|------|
| GPU 加速为空壳实现 | `tachyon-crypto` 的 `gpu` feature 当前仅编译通过，未完成实际 GPU 哈希管线 |
| QUIC 0-RTT 受 feature gate | 仅在 `http3` feature 启用时可用；0-RTT 被拒时透明回退 1-RTT |
| HTTP/HTTPS 代理与 BT SOCKS5 代理已支持 | `DownloadConfig.proxy`(及 `HTTP_PROXY`/`HTTPS_PROXY` 环境变量)与 `MagnetConfig.socks_proxy_url` 均已实现。**SEC-007**:代理模式下最终目标域名解析/可达 IP 由代理决定;本地 `PublicDnsResolver`/`reject_forbidden_ip` 不覆盖代理后端——代理即 SSRF 信任边界,仅使用可信代理。 |
| macOS io_uring 不可用 | macOS 不支持 io_uring，自动回退到 TokioFile |
| 前端仅支持中/英双语 | `solid-i18n` 当前仅配置 zh-CN 和 en-US |
| BitTorrent Magnet 已支持分片并发 | 单文件 magnet 走 `download_range_stream`（基于 librqbit `FileStream`）进入引擎多 worker 分片路径；多文件 magnet 仍回退整文件流式 |
| 路径授权非 openat2 | `validate_save_path` 拒绝中间 symlink/reparse 并在 open 前复查；**不**声称 `openat2(RESOLVE_BENEATH)` 句柄级路径封印，validate→open 仍存在 TOCTOU 残余 |
| ConnectionPool 非 TCP 池 | 历史名 `ConnectionPool` 实为并发许可器；TCP/TLS/H2 复用由 reqwest Client / `HttpClientRegistry` |
| quick-xml 间接漏洞 | CI 暂 ignore RUSTSEC-2026-0194/0195（librqbit-upnp + Tauri/plist 双链）；目标 quick-xml ≥0.41 后移除 |
| 配置热更新字段语义不统一 | `rate_limit`/`ConnectionPool`/`BufferPool`/`BtSession(magnet|download_dir)` 即时或新任务生效；剪贴板 `enable_watch` **即时**（轮询循环启动后按配置门禁）；`poll_interval_ms` 间隔热改仍为非目标；任务级 `retry_count` 当前恒 0（未聚合引擎 attempt） |

---

## 8. 贡献指南

### 8.1 提交 PR 流程

1. Fork 本仓库并创建特性分支。
2. 遵循 Rust 命名规范，代码标识符使用英文。
3. 注释、文档、提交信息使用中文。
4. 提交信息格式：`<类型>(<范围>): <简要描述>`。
5. 确保 `cargo clippy --all-targets --all-features -- -D warnings` 零警告。
6. 确保 `cargo fmt --all -- --check` 通过。
7. 新功能需附带测试，核心 crate 覆盖率不低于 90%。
8. 协议层改动需验证 `cargo build -p tachyon-protocol --no-default-features` 编译通过。
9. 所有 unsafe 代码必须有 Safety 注释。
10. 提交 PR 前运行本地 CI 预检命令全绿：

```bash
cargo fmt --all -- --check && \
  cargo clippy --all-targets --all-features -- -D warnings && \
  cargo nextest run --all && \
  cargo deny check && cargo audit && cargo machete && taplo check && \
  RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
```

### 8.2 本地开发建议

- 使用 `cargo nextest run --all` 替代 `cargo test`，并行执行更快。
- 修改 I/O 或调度相关代码后，先跑对应 bench 确认无性能回退。
- 引入并发优化前，必须用 `cargo bench` + `cargo crap` + `cargo llvm-cov` 交叉验证真实瓶颈。
- 会话开始时若 `target/` 超过 5GB，执行 `cargo clean`。

### 8.3 感谢贡献者

感谢所有提交 issue、PR 和建议的贡献者。
