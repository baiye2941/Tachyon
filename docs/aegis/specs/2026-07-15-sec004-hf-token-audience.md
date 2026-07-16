# Spec Brief：SEC-004 HF token 不向第三方镜像发送

## 范围
1. `HubApi` 仅当 endpoint host 为 `huggingface.co`（或官方 API host）时携带 `Authorization: Bearer`
2. Mirror / Race 浏览端点为 `hf-mirror.com` 时 **剥离 token**（匿名访问镜像）
3. `from_config` 与请求路径双重校验；单元测试覆盖

## 非目标
- 改变默认 Mirror 源策略
- Race 在有 token 时改走官方 list（产品策略，可后续）
- 下载 URL 注入 Authorization（当前下载路径不自动带 Hub token）

## TDD
strict
