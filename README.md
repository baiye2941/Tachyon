<p align="center">
  <img src="./assets/readme/hero.gif" width="100%" alt="Tachyon — Rust + Tauri v2 高性能桌面下载器：分片并发、BT/HF 原生、字节级进度">
</p>

<p align="center">
  <a href="https://github.com/baiye2941/Tachyon/actions/workflows/ci.yml?query=branch%3Amain+event%3Apush"><img src="https://img.shields.io/github/actions/workflow/status/baiye2941/Tachyon/ci.yml?branch=main&event=push&style=flat-square&logo=githubactions&logoColor=white&label=CI" alt="CI"></a>
  <a href="https://github.com/baiye2941/Tachyon/releases/latest"><img src="https://img.shields.io/github/v/release/baiye2941/Tachyon?style=flat-square&logo=github&label=release" alt="Release"></a>
  <img src="https://img.shields.io/badge/rust-1.85%2B-orange?style=flat-square&logo=rust" alt="Rust 1.85+">
  <img src="https://img.shields.io/badge/coverage-%E2%89%A590%25%20regions-brightgreen?style=flat-square" alt="Coverage ≥90% regions">
  <img src="https://img.shields.io/badge/clippy-0%20warnings-green?style=flat-square" alt="Clippy zero warnings">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=flat-square" alt="MIT / Apache-2.0"></a>
  <img src="https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-lightgrey?style=flat-square" alt="Windows Linux macOS">
</p>

<p align="center"><strong>大文件与 AI 模型的高速下载控制台</strong><br>
Rust 分片引擎 · Tauri v2 桌面壳 · BitTorrent SOCKS5 · HuggingFace Hub · 断点续传</p>

<p align="center">
  <a href="#安装">安装</a> ·
  <a href="#用法">用法</a> ·
  <a href="#系统架构">架构</a> ·
  <a href="#测试与质量">质量</a> ·
  <a href="docs/user-guide.md">用户指南</a> ·
  <a href="docs/architecture.md">架构文档</a>
</p>

---

## 它解决什么

Tachyon 面向大文件、AI 模型仓库与浏览器资源下载。目标不是堆概念，而是把**速度、续传可信度、协议可达性**做成同一条可验证路径。

### 速度从哪来

| 杠杆 | 具体做法 |
|------|----------|
| 分片并发 | `DownloadTask` 动态规划分片大小，`JoinSet` 多 worker 并行拉取 |
| 智能调度 | `AdaptiveDownloadScheduler` + `HoltLinearPredictor` 双指数平滑，按带宽与 RTT 调整分片策略 |
| 多源竞速 | `MirrorProtocol` least-in-flight + 质量加权选源，慢源不拖死整任务 |
| 平台级 I/O | Linux `io_uring`、Windows IOCP / WinFile，热路径少一次用户态绕行 |
| 流式校验 | BLAKE3 / SHA-256 边下边算，不在收尾再扫一遍大文件 |
| 限速可控 | 无锁令牌桶，进程内跨任务共享 `RateLimiter`，全速与限速切换不重建连接 |

### 痛点 → 结果

| 痛点 | Tachyon 的结果 |
|------|----------------|
| 大模型 / 大文件单线程吃不满带宽，断线后进度虚高 | 分片并发吃带宽；快照与分片索引一致性校验，失配归零重下，不虚报 |
| HF 模型要手拼 CDN 链接，缺仓库浏览 | 内置 Hub：模型浏览、LFS 解析、Token、本地模型库扫描 |
| 国内 BT 连不上 tracker / peer | SOCKS5 覆盖 tracker 与 peer；未配置时自动检测 `ALL_PROXY` / `HTTP_PROXY` |
| 只有总百分比，卡在哪片看不出来 | 分片矩阵约 250ms 推送字节级进度，快照式自愈 |
| 浏览器里的视频 / 文档资源难抓 | `tachyon-sniffer` 按扩展名识别并接入下载任务 |

---

<p align="center">
  <img src="./assets/readme/section-features.svg" width="100%" alt="核心能力">
</p>

| 能力 | 说明 |
|------|------|
| 多线程分片下载 | `DownloadTask` 动态分片规划，`JoinSet` 并发执行 |
| 分片字节级实时进度 | 详情页分片矩阵约 250ms 聚合推送，快照式自愈 |
| 多协议传输 | HTTP/HTTPS、BitTorrent 磁力链接；QUIC/HTTP3 为可选 feature |
| BT 代理支持 | SOCKS5 覆盖 tracker 与 peer；未配置时自动检测 `ALL_PROXY` / `HTTP_PROXY` |
| 高性能存储引擎 | Linux io_uring、Windows IOCP / WinFile、TokioFile 自动回退 |
| 智能调度 | `AdaptiveDownloadScheduler` + `HoltLinearPredictor` 双指数平滑 |
| 多源并发下载 | `MirrorProtocol` least-in-flight 调度 + 质量加权选源 |
| 断点续传 | 任务快照持久化，分片级与字节级续传，快照一致性校验 |
| 流式哈希校验 | BLAKE3 / SHA-256 CPU 流式校验 |
| 限速控制 | 无锁令牌桶，支持跨任务全局限速 |
| HuggingFace Hub | 模型浏览、LFS 解析、Token 管理、本地模型扫描 |
| 浏览器资源嗅探 | 基于扩展名识别视频 / 音频 / 文档 / 压缩包等资源 |

---

<p align="center">
  <img src="./assets/readme/workflow.svg" width="100%" alt="下载路径：Probe → Plan → Execute → Write+Hash → Resume">
</p>

真实下载路径走 `DownloadTask::probe → plan → execute`，不是模拟下载。

---

<p align="center">
  <img src="./assets/readme/section-install.svg" width="100%" alt="安装与开发">
</p>

### 预构建安装包（推荐）

从 [GitHub Releases](https://github.com/baiye2941/Tachyon/releases/latest) 下载最新版：

- **Windows**：`.msi` / setup.exe
- **macOS**：`.dmg`（当前发布 aarch64）
- **Linux**：`.deb` / `.rpm` / `.AppImage`

安装包附带 Tauri updater `.sig`、`.sha256` 与 cosign 校验材料。

### 从源码构建

| 依赖 | 最低 / 说明 |
|------|-------------|
| Rust | MSRV **1.85**；`rust-toolchain.toml` 使用 `stable` + rustfmt/clippy |
| Bun | 1.x |
| Tauri CLI | 2.x（`cargo tauri`） |

```bash
git clone https://github.com/baiye2941/Tachyon.git
cd Tachyon

# 调试构建（默认开启 HTTP + magnet）
cargo build

# 发布构建
cargo build --release

# 同时启动前端 + Rust 后端
cargo tauri dev
```

QUIC/HTTP3（reqwest http3 为 unstable，需显式开启）：

```bash
RUSTFLAGS='--cfg reqwest_unstable' cargo build --features tachyon-protocol/http3
```

前端开发：

```bash
cd frontend && bun install && bun run dev
```

---

## 用法

### GUI 快速开始

1. 启动应用：`cargo tauri dev` 或运行 Release 安装包。
2. 在「新建任务」中粘贴下载链接，或从 HuggingFace Hub 浏览模型。
3. 选择保存路径，点击下载；任务列表实时显示速度、进度与分片状态。
4. 打开任务详情页可查看分片矩阵：下载中分片显示字节级进度与百分比。
5. 支持暂停、恢复、取消、删除；重启后会自动恢复未完成任务。

### HuggingFace 模型下载

在 HF 浏览器面板输入模型 ID（如 `bert-base-uncased`），选择分支与文件后批量创建下载任务。访问私有仓库时：

```bash
export HF_TOKEN=your_token_here
```

### BT / 磁力链接

设置页「磁力链接」可配置 SOCKS5 代理（覆盖 tracker 与 peer）。留空时自动检测 `ALL_PROXY` / `HTTP_PROXY` 并转为 `socks5://`。

国内网络下 BT 通常需要 SOCKS5；仅配置 `HTTP_PROXY` 往往不够（UDP tracker / DHT / peer TCP 仍可能直连失败）。

### 配置要点

- `download_dir`：默认下载目录
- `max_concurrent_fragments`：单任务并发分片数
- `max_retries`：分片失败重试次数
- `rate_limit_bytes_per_sec`：全局限速
- `io_strategy`：I/O 后端策略
- `magnet.socks_proxy_url`：BT SOCKS5 代理

完整配置见 [docs/user-guide.md](docs/user-guide.md)。

---

## 技术栈

### 前端

| 技术 | 版本 | 用途 |
|------|------|------|
| SolidJS | ^1.9.13 | 细粒度响应式 UI |
| Tauri API | ^2.11.0 | 前后端 IPC |
| TailwindCSS | ^4.3.1 | 原子化 CSS |
| Vite | ^8.1.0 | 构建工具 |
| Bun | 1.x | 包管理与脚本运行 |
| Vitest / Playwright / Storybook | — | 测试与组件开发 |

### 后端（Rust workspace 10 crate）

| Crate | 职责 |
|------|------|
| `tachyon-core` | 核心类型、trait、错误体系、配置、安全校验 |
| `tachyon-engine` | 分片引擎、连接管理、多源竞速、限速、任务执行 |
| `tachyon-scheduler` | 智能调度、带宽预测、优先级队列 |
| `tachyon-io` | 跨平台异步 I/O（io_uring / IOCP / WinFile）、`BufferPool` |
| `tachyon-protocol` | HTTP/HTTPS、BitTorrent（默认 magnet）、可选 HTTP3 |
| `tachyon-crypto` | BLAKE3 / SHA-256 CPU 流式哈希与完整性校验 |
| `tachyon-sniffer` | 浏览器资源类型识别与捕获过滤 |
| `tachyon-store` | 断点续传快照、文件系统 KV |
| `tachyon-hub` | HuggingFace Hub API 客户端 |
| `tachyon-app` | Tauri 应用入口、IPC 命令、生命周期管理 |

---

## 系统架构

<p align="center">
  <img src="./assets/readme/architecture.svg" width="100%" alt="Tachyon 单向依赖分层架构">
</p>

依赖层序（禁止跨层绕行）：

`tachyon-core` → `{tachyon-protocol, tachyon-io, tachyon-crypto, tachyon-scheduler}` → `tachyon-engine` → `tachyon-app`；`tachyon-hub` / `tachyon-sniffer` / `tachyon-store` 按各自 `Cargo.toml` 依赖。

详细架构见 [docs/architecture.md](docs/architecture.md)。

---

<p align="center">
  <img src="./assets/readme/section-quality.svg" width="100%" alt="测试与质量">
</p>

当前仓库约有：

- **Rust**：`#[test]` / `#[tokio::test]` 属性约 **2090+**
- **前端**：约 **87** 个 Vitest 规格文件、约 **900+** 用例

CI 门禁覆盖构建 / 格式 / Clippy / 测试 / 覆盖率 / 审计 / 前端等；`test` job 跑 Windows / Linux / macOS。

```bash
# Rust 测试（nextest）
cargo nextest run --all

# Clippy 零警告
cargo clippy --all-targets --all-features -- -D warnings

# 覆盖率门禁（逐 crate + regions ≥ 90）
bash scripts/ci/coverage.sh

# 前端测试
cd frontend && bun run test
```

---

## 贡献指南

1. Fork 并创建特性分支。
2. 代码标识符使用英文；注释、文档、提交信息使用中文。
3. 提交信息格式：`<类型>(<范围>): <简要描述>`。
4. 确保 `cargo clippy --all-targets --all-features -- -D warnings` 零警告。
5. 新功能需附带测试，核心 crate 覆盖率不低于 90% regions。
6. 所有 `unsafe` 代码必须有 Safety 注释。

更多细节见 [docs/user-guide.md](docs/user-guide.md) 与 [AGENTS.md](AGENTS.md)。

---

## 主要维护者

- [@baiye2941](https://github.com/baiye2941)

---

## 开源协议

本项目采用 MIT / Apache-2.0 双许可证。详见 [LICENSE](LICENSE) 与 [LICENSE-APACHE](LICENSE-APACHE)。

<!-- 静态 fallback：若 GIF 无法播放，可改用 ./assets/readme/hero.svg -->
