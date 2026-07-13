# 审计证据包

## 审计基线

- Git 基线：`27c3224ce218fddef6ce0e6f7c216bd447d27417`。
- 审计开始前 `target/` 超过项目阈值，已按项目规则执行 `cargo clean`；之后验证重新生成构建产物。
- 结束时 `git diff --quiet` 成功；已跟踪产品文件无差异。
- 审计报告为被 `.gitignore` 忽略的本地文件：`Document/Tachyon系统性深度审计报告-2026-07-13.md`。

## 静态与交叉复核证据

- 高风险源码结论的主要位置：
  - `crates/tachyon-io/src/aligned_buf.rs:255-274`：`split()` 共享分配并重置原缓冲区位置。
  - `crates/tachyon-io/src/iouring.rs:716-768`：取消时固定缓冲区索引归还。
  - `crates/tachyon-engine/src/downloader.rs:1606-1645`：工作窃取先修改拓扑、后忽略 `try_send` 结果。
  - `crates/tachyon-engine/src/downloader.rs:1691-1720`：完成发送器取走后，窃取路径仍解包。
  - `crates/tachyon-engine/src/downloader.rs:1124-1208`：整流单次写入与未知大小完成。
  - `crates/tachyon-protocol/src/http.rs:528-551,1130-1137,1191-1202`：`206` 的 `Content-Range` 放行。
  - `crates/tachyon-engine/src/downloader.rs:292-325`：HTTP/磁力协议选择，未接入 HLS。
  - `crates/tachyon-protocol/src/hls.rs:366-502`：HLS 估算大小、完整/流式 API 差异。
- 规范依据：RFC 9110、RFC 8216、RFC 9113、RFC 1928、BEP 3/5/15 与 fio 文档的不可变快照保存于外部审计目录 `source_snapshots/`。

## 已执行验证

| 命令或检查 | 结果 | 覆盖边界 |
|---|---|---|
| `cargo fmt --all -- --check` | 通过 | Rust 格式。 |
| `cargo build --all --locked` | 通过 | 全工作区锁定构建。 |
| `cargo clippy --all-targets --all-features --locked -- -D warnings` | 通过 | Clippy 零警告。 |
| `cargo nextest run --all --locked --retries 0` | 1,510/1,510 | 单次本机测试。 |
| `TACHYON_BENCH_MODE=smoke cargo test --benches --locked` | 通过 | 基准代码可运行；非公网性能证明。 |
| Bun audit/typecheck/lint/test/build | 通过 | 前端静态与单元构建表面。 |
| `cargo deny check`、带忽略项 `cargo audit` | 通过 | 不消除已忽略 advisory 和维护状态风险。 |
| LLVM 覆盖率 | 部分失败 | engine 88.91% regions；core profiler 合并访问拒绝。 |
| Playwright | 未完成 | 缺 Chromium，且不是原生 Tauri E2E。 |

## 外部台账完整性

目录：`C:/Users/白夜/Documents/Tachyon_Deep_Audit_20260713/`

- `sources.jsonl`：18 条 UTF-8 可读来源。
- `evidence.jsonl`：37 条 UTF-8 可读证据。
- `claims.jsonl`：14 条经复核事实性主张。
- 严格支持验证：0 条事实主张无来源；10 条 `supported`、3 条 `partial`、1 条 `needs_review`。
- 中文报告 SHA-256：`8b68ad1d2ac475bb07a975f909f18bbd342c59b83c234cdceef9877e76161c7b`。

## 未覆盖范围

Linux `io_uring` 真实取消竞争、公网 TLS/CDN、BT swarm/DHT/UDP/SOCKS、HLS 直播、断电耐久性、Windows 重解析点和原生 Tauri E2E 均未在本次本机审计中动态验证。报告中将其标为条件成立或未验证，不得外推为已通过或已复现。
