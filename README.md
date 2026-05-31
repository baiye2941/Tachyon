# AI Model Downloader

> 基于 Rust + Tauri 构建的新一代超高性能下载器。

## 核心能力

- **多线程分片下载** -- 自动将文件拆分为多个分片并行下载,最大化带宽利用率
- **超分片引擎** -- 动态粒度分片,根据带宽反馈实时调整分片大小与并发数
- **动态连接管理** -- 智能维护连接池,自动扩缩连接数以适配网络状况
- **QUIC + 0-RTT** -- 基于 QUIC 协议实现零往返时间建连,降低连接延迟
- **MP-QUIC 多路径传输** -- 单连接多路径传输,自动在 WiFi/5G/有线间切换与聚合
- **零拷贝存储引擎** -- 通过 io_uring 实现网络收包到文件写入全程无用户态拷贝
- **P2SP 混合下载** -- CDN + DHT Peer 双源下载,自动测速选择最优源
- **GPU 加速校验** -- 通过 Vulkan Compute 或 CUDA 对分片做并行哈希校验
- **内核网络栈旁路** -- Linux 下可选 XDP/AF_XDP 旁路内核协议栈,进一步提升吞吐
- **智能调度与预测** -- 基于历史数据的带宽预测模型,提前分配连接资源
- **协议级优化** -- 支持 HTTP/HTTPS/QUIC/FTP 等多协议,针对每种协议做专项优化
- **浏览器资源嗅探** -- 通过 Playwright MCP 拦截浏览器流量,自动捕获下载资源

## 快速开始

### 环境要求

| 依赖       | 最低版本 | 说明                     |
|------------|----------|--------------------------|
| Rust       | 1.85+    | edition 2024             |
| Bun        | 最新     | 前端包管理与构建         |
| Node.js    | 18+      | Tauri CLI 依赖           |
| cargo-tauri| 2.x      | Tauri 开发与构建工具     |

### 安装与构建

```bash
# 克隆仓库
git clone https://github.com/user/AI-Model-Downloader.git
cd AI-Model-Downloader

# 构建(调试模式)
cargo build

# 构建(发布模式,启用 LTO 和全量优化)
cargo build --release
```

### 开发模式

```bash
# 安装前端依赖并启动前端开发服务器
cd frontend && bun install && bun run dev

# 启动 Tauri 开发模式(同时启动前端 + Rust 后端)
cargo tauri dev
```

### AI 辅助开发(可选)

本项目包含 Claude Code 智能体配置,提供 Rust 代码审查、TDD 引导、架构设计等 AI 辅助能力:

```bash
# 安装 Claude Code 后,进入项目目录即可自动加载配置
claude
```

`.claude/` 目录中的技能(skills)和智能体(agents)会自动注入至 AI 会话,提供 Rust 模式检查、性能审查、测试策略指导等上下文。

## 架构

### 分层架构

```
+------------------------------------------+
|  GUI (Tauri + 前端)                       |
+------------------------------------------+
|  amd-app        应用层:任务管理、配置、UI 绑定  |
+------------------------------------------+
|  amd-scheduler  调度层:智能调度、带宽分配、预测   |
+------------------------------------------+
|  amd-engine     引擎层:分片引擎、连接池、P2SP   |
+------------------------------------------+
|  amd-protocol   协议层:HTTP/HTTPS/QUIC/FTP   |
+------------------------------------------+
|  amd-io         I/O 层:io_uring、零拷贝、落盘   |
+------------------------------------------+
|  amd-sniffer    嗅探层:浏览器资源拦截与解析      |
+------------------------------------------+
|  amd-crypto     校验层:GPU 加速哈希、完整性校验  |
+------------------------------------------+
```

### 模块说明

| Crate         | 职责                                        | 关键技术                        |
|---------------|---------------------------------------------|---------------------------------|
| `amd-core`     | 核心类型、trait 定义、错误体系、配置与事件   | thiserror, serde                |
| `amd-engine`   | 分片引擎、连接管理、MP-QUIC 多路径传输       | tokio, quinn, bytes             |
| `amd-scheduler`| 智能调度器、带宽预测、优先级队列             | Holt-Winters, BinaryHeap        |
| `amd-io`       | 跨平台异步文件 I/O、缓冲区池管理             | tokio, bytes, io-uring(可选)    |
| `amd-protocol` | HTTP/HTTPS/QUIC/FTP 协议适配与实现           | reqwest, quinn, url             |
| `amd-sniffer`  | 浏览器资源嗅探、流量拦截与解析               | url, playwright MCP             |
| `amd-crypto`   | CPU/GPU 哈希校验、完整性验证                 | blake3, sha2, wgpu(可选)        |
| `amd-p2sp`     | P2SP 混合下载、DHT 网络、Peer 发现           | 自研 Kademlia DHT, Xorshift64   |
| `amd-app`      | Tauri 应用入口、命令注册、GUI 事件桥接       | tauri v2                        |

## 项目结构

```
AI-Model-Downloader/
  Cargo.toml              # workspace 根配置
  LICENSE                 # MIT 许可证
  README.md               # 项目说明(本文件)
  .claude/                # AI 辅助配置(skills + agents)
  crates/
    amd-core/              # 核心类型与 trait 定义
    amd-engine/            # 分片引擎与连接管理
    amd-scheduler/         # 智能调度器
    amd-io/                # 跨平台异步文件 I/O
    amd-protocol/          # 多协议适配
    amd-sniffer/           # 浏览器资源嗅探
    amd-crypto/            # CPU/GPU 哈希校验
    amd-p2sp/              # P2SP 混合下载
    amd-app/               # Tauri 应用入口
  frontend/               # Tauri 前端(Bun)
  tests/                  # 集成测试
  benches/                # criterion 基准测试(3组)
  docx/                   # 参考文档(本地,不提交)
```

## 测试

```bash
# 运行全部测试
cargo test --all

# 运行指定 crate 的单元测试
cargo test -p amd-core --lib

# 运行指定测试(精确匹配)
cargo test -p amd-core -- test_name --exact

# 代码检查(clippy 零警告)
cargo clippy --all-targets --all-features -- -D warnings

# 格式检查
cargo fmt --all -- --check

# 测试覆盖率(目标 95%)
cargo llvm-cov --fail-under-lines 95
```

### 测试策略

项目采用六类测试覆盖:

1. **正常路径** -- 核心功能的预期行为验证
2. **空值处理** -- 空输入、缺失字段的健壮性
3. **边界值** -- 极大/极小输入、整数溢出等边界条件
4. **并发安全** -- 多线程/异步竞态条件检测
5. **外部故障** -- 网络超时、IO 错误等异常模拟
6. **恶意输入** -- 超长字符串、非法路径等安全测试

使用 `proptest` 进行属性测试,`tokio::test` 进行异步测试,`mockall` 隔离外部依赖。

项目结构代码检查清单: `cargo clippy` (零警告)、`cargo fmt` (合规)、`cargo test` (通过)。

## 基准测试

```bash
# 运行全部基准测试
cargo bench
```

项目包含以下基准测试:

| 基准测试            | 测量内容                    |
|---------------------|-----------------------------|
| `buffer_pool`       | 缓冲区池分配与回收性能      |
| `crypto_hash`       | BLAKE3/SHA-256 哈希计算吞吐 |
| `fragment_planning` | 分片规划算法效率            |

## 技术栈

| 功能         | Crate                          | 说明                              |
|--------------|--------------------------------|-----------------------------------|
| 异步运行时   | `tokio`                        | multi-thread, full features       |
| QUIC 协议    | `quinn`                        | 基于 rustls 的 QUIC 实现          |
| io_uring     | `io-uring` / `tokio-uring`     | Linux 异步 IO 接口(按需启用)      |
| HTTP 客户端  | `reqwest`                      | 基于 hyper,支持 rustls-tls        |
| 桌面框架     | `tauri` v2                     | 跨平台桌面应用框架                |
| GPU 计算     | `wgpu` / `vulkano`             | WebGPU / Vulkan Compute(预留)     |
| 哈希算法     | `blake3`, `sha2`               | 高性能哈希与校验                  |
| 序列化       | `serde`, `serde_json`          | JSON 与结构化数据序列化           |
| 错误处理     | `thiserror`                    | 结构化错误体系                    |
| 日志         | `tracing`                      | 结构化日志与过滤                  |
| 属性测试     | `proptest`                     | 基于属性的随机测试                |
| 基准测试     | `criterion`                    | 统计学基准测试框架                |
| Mock 框架    | `mockall`                      | trait 与函数 mock                 |
| 时间序列预测 | Holt-Winters(自研)             | 带宽预测模型                      |
| Tauri 测试   | `serial_test`                  | 全局 Mutex 隔离测试               |

### 发布构建优化

```toml
[profile.release]
opt-level = 3       # 最高优化级别
lto = true          # 链接时优化
codegen-units = 1   # 单编译单元(更优的内联与优化)
strip = true        # 剥离符号表(减小二进制体积)
```

## 贡献指南

1. Fork 本仓库并创建特性分支
2. 遵循 Rust 命名规范,代码标识符使用英文
3. 注释、文档、提交信息使用中文
4. 提交信息格式: `<类型>(<范围>): <简要描述>`
5. 确保 `cargo clippy --all-targets --all-features -- -D warnings` 零警告
6. 确保 `cargo fmt --all -- --check` 通过
7. 新功能需附带测试,覆盖率不低于 95%
8. 提交 Pull Request 前运行 `cargo test --all` 确保全部通过

## 项目状态

| 指标        | 状态 |
|-------------|------|
| 编译       | `cargo check` 通过,零错误 |
| 代码质量   | `cargo clippy` 零警告 |
| 测试       | 单元 + 集成 + 属性测试全部通过 |
| 覆盖率目标 | 95%+(行覆盖率) |
| Crate 数量 | 9 个 workspace crate |
| 基准测试   | 3 组 criterion 基准测试 |

## 许可证

本项目采用 MIT 或 Apache-2.0 双许可,可任选其一。详见 [LICENSE](LICENSE)。
