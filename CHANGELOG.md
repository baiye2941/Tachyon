# Changelog

本文件记录 Tachyon 面向用户的版本变更。

## [0.1.2] — 2026-07-20

### 安全与发布

- 启用真实 Tauri updater ed25519 签名（`createUpdaterArtifacts` + 非 PLACEHOLDER pubkey）
- 发布链路强制：私钥 secret 缺失失败、构建后 `.sig` 硬断言、SHA256/cosign 递归签名与数量断言
- Release 失败真回滚：已 public 尝试 re-draft，失败则 delete（禁止仅 prerelease 当回滚）
- 发布使用 GitHub Environment `release-production`
- 发布附带 SBOM 清单（cargo tree + frontend lock）并尝试 cosign

### CI

- 门禁 SSOT：`scripts/ci/{miri,coverage,audit,version-check,sign-release,preflight,check-doc-drift}.sh`
- 覆盖率：一次 instrument + 逐 crate regions ≥ 90
- path filter / composite setup / rust-cache shared-key / bench 移出关键路径
- Release 绑定同 SHA 的 CI 绿；失败才 fallback 全量门禁
- dry-run 可接线；`dry-run=false` 仅允许 tag ref
- 文档漂移检测 job

### 修复

- `ci-pass` 假绿（`needs.*.result` 不展开）
- max_concurrent 测试全局 store 锁竞态
- release.yml 含冒号 unquoted run 导致 YAML 解析失败

## [0.1.1-0] — 2026-07-04

- 既有发布基线（见 Git 历史）

## [0.2.0] — 2026-05-31

- 历史里程碑 tag（amd-hub / P2SP 等，见 Git 历史）
