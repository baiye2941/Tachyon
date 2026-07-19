#!/usr/bin/env bash
# SSOT: 覆盖率门禁（逐 crate + --fail-under-regions 90）
# 禁止改回合计 fail-under-lines；HTML 可选，不参与门禁。
set -euo pipefail

IGNORE='(test_harness|iocp|winio|iouring)'
CRATES=(tachyon-core tachyon-engine tachyon-store tachyon-io tachyon-crypto tachyon-scheduler)

# 仅生成 HTML（供 CI always 步骤，不重跑门禁）
if [[ "${COVERAGE_HTML_ONLY:-0}" == "1" ]]; then
  pkg_args=()
  for crate in "${CRATES[@]}"; do
    pkg_args+=(-p "$crate")
  done
  cargo llvm-cov "${pkg_args[@]}" --locked \
    --ignore-filename-regex "$IGNORE" --html || true
  exit 0
fi

for crate in "${CRATES[@]}"; do
  echo "::group::覆盖率: $crate"
  cargo llvm-cov -p "$crate" --locked \
    --ignore-filename-regex "$IGNORE" \
    --fail-under-regions 90 --summary-only
  echo "::endgroup::"
done

if [[ "${COVERAGE_HTML:-0}" == "1" ]]; then
  pkg_args=()
  for crate in "${CRATES[@]}"; do
    pkg_args+=(-p "$crate")
  done
  cargo llvm-cov "${pkg_args[@]}" --locked \
    --ignore-filename-regex "$IGNORE" --html || true
fi
