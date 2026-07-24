#!/usr/bin/env bash
# SSOT: 变异测试门禁(审计 E-01)
#
# cargo-mutants 极慢(每个变异体需编译+跑测试),不适合 PR 级 CI。
# 仅在 main push / schedule 运行,不进 ci-pass 关键路径。
#
# 策略:仅变异核心逻辑 crate(tachyon-core),限制并发与超时,
# 排除测试/基准代码本身。
set -euo pipefail

# cargo-mutants v27+: --in-place 与 --jobs/-j 互斥。
# --in-place 在工作树直接变异(CI checkout 可丢弃),默认并行度由工具自行调度。
cargo mutants --in-place \
  -p tachyon-core \
  --exclude 'tests/**' \
  --exclude 'benches/**' \
  --timeout 300
