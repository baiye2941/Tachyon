# Spec Brief：SEC-008 日志脱敏 · 官方 HF 下载 Authorization

## 范围
1. **SEC-008**：引擎/app/sniffer 中仍打印完整 URL 的 info/instrument/debug 点，统一 `redact_url_for_log`
2. **HF 下载 token**：`HttpClient` 持有可选 `auth_bearer`，**仅当请求 host 为 huggingface.co(或子域)** 时附加 `Authorization`；镜像 host 永不发送
3. 会话构建：从 `HubConfig.token` 注入到下载用 `HttpClient`（不经 `DownloadConfig.headers`，避免绕过 reserved 黑名单的产品路径）
4. **SEC-005**：`AlignedBuf` 已 COW + 回归测试，本轮仅验证并标记闭环

## 非目标
- 把 HF token 写入 `config.json` / 备份
- Race 下强制官方主源
- 改默认 Mirror 策略

## TDD
strict
