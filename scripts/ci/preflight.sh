#!/usr/bin/env bash
# SSOT: 本地 CI 预检入口（与 .github/workflows/ci.yml 门禁同源）
# 用法:
#   bash scripts/ci/preflight.sh --quick   # fmt/clippy/nextest/deny/audit/taplo/doc
#   bash scripts/ci/preflight.sh --full    # quick + coverage + frontend（MIRI=1 时再跑 miri）
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

mode="quick"
for arg in "$@"; do
  case "$arg" in
    --quick) mode="quick" ;;
    --full) mode="full" ;;
    -h|--help)
      cat <<'EOF'
用法: bash scripts/ci/preflight.sh [--quick|--full]

  --quick  默认。fmt + clippy + nextest + deny + audit + taplo + doc
  --full   quick + coverage + frontend；设 MIRI=1 时再跑 miri
EOF
      exit 0
      ;;
    *)
      echo "error: 未知参数 '$arg'（期望 --quick|--full）" >&2
      exit 2
      ;;
  esac
done

run_step() {
  local name="$1"
  shift
  echo ""
  echo "======== preflight: $name ========"
  "$@"
}

run_step "cargo fmt --check" cargo fmt --all -- --check
run_step "cargo clippy" cargo clippy --all-targets --all-features --locked -- -D warnings
run_step "cargo nextest" cargo nextest run --all --locked
run_step "cargo deny" cargo deny check
run_step "cargo audit (SSOT)" bash scripts/ci/audit.sh
if command -v taplo >/dev/null 2>&1; then
  run_step "taplo check" taplo check
else
  echo "======== preflight: taplo check ========"
  echo "warning: 未安装 taplo，跳过（CI 仍会检查）" >&2
fi
run_step "cargo doc" env RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features --locked

if [[ "$mode" == "full" ]]; then
  run_step "coverage (SSOT)" bash scripts/ci/coverage.sh
  if [[ -d frontend ]]; then
    run_step "frontend install" bash -c 'cd frontend && bun install --frozen-lockfile'
    run_step "frontend audit" bash -c 'cd frontend && bun audit --audit-level=high'
    run_step "frontend typecheck" bash -c 'cd frontend && bun run typecheck'
    run_step "frontend lint" bash -c 'cd frontend && bun run lint'
    run_step "frontend test" bash -c 'cd frontend && bun run test'
    run_step "frontend build" bash -c 'cd frontend && bun run build'
  fi
  if [[ "${MIRI:-0}" == "1" ]]; then
    run_step "miri (SSOT)" bash scripts/ci/miri.sh
  else
    echo "======== preflight: miri ========"
    echo "info: 跳过 miri（设 MIRI=1 启用）"
  fi
fi

echo ""
echo "preflight ($mode) 全部通过"
