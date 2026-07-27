# Changelog

本文件记录 Tachyon 面向用户的版本变更。


## [0.1.3] — 2026-07-27

### 性能

- `default_target_fragments` 16→64，解除中等文件分片数=并发上限的队列深度归零
- rebalance 对齐 IDM：空闲 worker 触发、最大剩余字节、对半拆分、收尾 500ms 冷却
- bench harness 支持源内 slow-zone，便于 straggler 场景复现

### 崩溃一致性

- `Loose` 改为 group-commit（N=8），不再在分片完成边界完全跳过 sync
- mid-flight partial 上报前 durable（EveryFragment 每次 / Loose 每 2 次）
- OS kill + resume 全文件 blake3 冒烟；PageCache/真实文件 resume 证据链

### 修复

- 删除 plan 阶段 `confidence>0` 不可达分支
- 前端 bun audit：brace-expansion≥5.0.8、postcss≥8.5.18
- CI flaky：magnet seeder 临时端口、mirror 锁开销阈值、store durable 写失败注入

### 工程

- tachyon-engine regions 覆盖率 ≥90%（90.46%）
- hybrid/magnet 构造与 storage multi sync 测试补齐

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
