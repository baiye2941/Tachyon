#!/usr/bin/env bash
# SSOT: 覆盖率门禁（一次收集 + 逐 crate regions >= 90）
# 禁止改回合计 fail-under-lines；禁止 6 次独立 instrument 循环。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

IGNORE='(test_harness|iocp|winio|iouring)'
CRATES=(tachyon-core tachyon-engine tachyon-store tachyon-io tachyon-crypto tachyon-scheduler)
THRESHOLD="${COVERAGE_THRESHOLD:-90}"
JSON_OUT="${COVERAGE_JSON:-target/llvm-cov/coverage.json}"

pkg_args=()
for crate in "${CRATES[@]}"; do
  pkg_args+=(-p "$crate")
done

crates_csv=$(IFS=,; echo "${CRATES[*]}")

# 仅生成 HTML（供 CI always 步骤；优先 report 复用，失败则整包重跑）
if [[ "${COVERAGE_HTML_ONLY:-0}" == "1" ]]; then
  if cargo llvm-cov report --html 2>/dev/null; then
    exit 0
  fi
  cargo llvm-cov "${pkg_args[@]}" --locked \
    --ignore-filename-regex "$IGNORE" --html || true
  exit 0
fi

mkdir -p "$(dirname "$JSON_OUT")"

echo "::group::覆盖率: 一次 instrument（${#CRATES[@]} crates）"
cargo llvm-cov "${pkg_args[@]}" --locked \
  --ignore-filename-regex "$IGNORE" \
  --json --summary-only \
  --output-path "$JSON_OUT"
echo "::endgroup::"

echo "::group::覆盖率: 逐 crate regions 断言 (>= ${THRESHOLD}%)"
# GHA Ubuntu 有 python3；本地 Windows 常见仅有 python
if command -v python3 >/dev/null 2>&1; then
  PY=python3
elif command -v python >/dev/null 2>&1; then
  PY=python
else
  echo "::error::需要 python3 或 python 以运行 coverage_assert.py" >&2
  exit 2
fi
"$PY" "$ROOT/scripts/ci/coverage_assert.py" "$JSON_OUT" \
  --threshold "$THRESHOLD" \
  --crates "$crates_csv" \
  --ignore-regex "$IGNORE"
echo "::endgroup::"

if [[ "${COVERAGE_HTML:-0}" == "1" ]]; then
  echo "::group::覆盖率: HTML 报告"
  if ! cargo llvm-cov report --html; then
    cargo llvm-cov "${pkg_args[@]}" --locked \
      --ignore-filename-regex "$IGNORE" --html || true
  fi
  echo "::endgroup::"
fi
