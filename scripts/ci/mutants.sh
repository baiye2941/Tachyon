#!/usr/bin/env bash
# SSOT: 变异测试门禁(审计 E-01)
#
# 由独立 workflow `.github/workflows/mutants.yml` 在 main push / schedule
# 运行,不进主 CI,不污染 README CI badge。
#
# 历史失败:
# 1. 全量 tachyon-core 变异面约 800+,GHA 2h 内跑不完 → cancelled
# 2. 大量噪声 MISSED 导致 cargo-mutants 非零退出 → failure
# 3. cargo-mutants v27+: --in-place 与 -j 互斥
#
# 现行策略:
# - 默认读 `.cargo/mutants.toml`:仅 safety/ 关键路径(路径/SSRF)
# - 复制树 + 并行 jobs(非 --in-place)
# - MODE=full 可全量(长任务)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

MODE="${TACHYON_MUTANTS_MODE:-ci}"
JOBS="${CARGO_MUTANTS_JOBS:-4}"
TIMEOUT="${TACHYON_MUTANTS_TIMEOUT:-120}"

# 清理上次产物,避免 rename mutants.out → .old 权限问题
rm -rf mutants.out mutants.out.old 2>/dev/null || true

common_args=(
  -p tachyon-core
  --timeout "${TIMEOUT}"
  -j "${JOBS}"
)

case "${MODE}" in
  ci)
    echo "==> mutants CI mode: safety/ only (.cargo/mutants.toml), jobs=${JOBS}"
    cargo mutants -p tachyon-core --list | tee mutants-list.txt >/dev/null
    echo "mutant_count=$(wc -l < mutants-list.txt | tr -d ' ')"
    cargo mutants "${common_args[@]}"
    ;;
  full)
    echo "==> mutants FULL mode: entire tachyon-core (excl test_harness), jobs=${JOBS}"
    cargo mutants "${common_args[@]}" \
      --no-config \
      --exclude '**/test_harness.rs' \
      --exclude '**/tests/**' \
      --exclude '**/benches/**'
    ;;
  list)
    echo "==> listing mutants under current config"
    cargo mutants -p tachyon-core --list
    ;;
  *)
    cat <<'USAGE'
用法: TACHYON_MUTANTS_MODE=ci|full|list bash scripts/ci/mutants.sh

  ci    (默认) safety/ 关键路径门禁,读 .cargo/mutants.toml
  full  全量 tachyon-core(本地/长任务)
  list  仅列出将要跑的变异

环境变量:
  CARGO_MUTANTS_JOBS         并行 jobs(默认 4)
  TACHYON_MUTANTS_TIMEOUT    单变异测试超时秒(默认 120)
  TACHYON_MUTANTS_MODE       ci|full|list
USAGE
    exit 1
    ;;
esac
