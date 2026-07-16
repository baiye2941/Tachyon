# Spec Brief：审计 residual 正确性批次（HTTP-04 / HTTP-01 / HTTP-11 / HTTP-10）

## TaskIntent
继续按 `Document/PI/Tachyon-Deep-Audit-2026-07-14` Confirmed 项修复，不重开产品方向。

## 本批范围（顺序）
1. **HTTP-04**：`probe_via_get_range` 的 206 必须 `validate_content_range(headers, 0, 0)`；失败不得宣称 `supports_range=true`
2. **HTTP-01**：分片 retry 每次 attempt 开始 `write_buf.clear()`，半缓冲失败不得污染下次写入
3. **HTTP-11**：reqwest 启用 `socks` feature；或在未启用时拒绝 `socks5://` 配置并给出明确 Config 错误（本批选启用 feature，与 validate 已允许 socks5 对齐）
4. **HTTP-10**：`DownloadConfig.user_agent` 与安全允许的 `headers` 进入 HTTP 请求；禁止覆盖 `Range`/`Host`/`Content-Length` 等协议头

## 非目标
- 跨任务 `reqwest::Client` registry（HTTP-15，架构更大，下一批）
- live HLS / MAP / fMP4
- QUIC prior knowledge 产品策略

## 设计选择
| 项 | 选择 | 理由 |
|---|---|---|
| HTTP-04 | 206 分支先 validate 再 metadata | 与 range 下载同构 |
| HTTP-01 | retry loop 内、调用 download_single_fragment 前 clear | 最小改动、与审计最小修复一致 |
| HTTP-11 | workspace reqwest 加 `socks` feature | 配置层已允许 socks5，诚实能力 |
| HTTP-10 | build_client 用 config.user_agent；请求层 merge headers 并过滤 reserved | 防协议头覆盖 |

## TDD Route
- Mode: auto → **strict**
- Verification: nextest 相关 + clippy -D warnings

## ArchitectureReviewRequired
yes（HTTP-10/11 触及配置→协议边界）

## 用户授权
用户指令：「继续修复，不要一直问，按照报告修复就是了」+ TDD + 头脑风暴 + 多 Agent 交叉验证。
