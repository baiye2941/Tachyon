#!/usr/bin/env bash
# SSOT: cargo-audit ignore 列表唯一来源 = deny.toml [advisories].ignore
# 禁止在 workflow YAML 手写第三份 RUSTSEC 列表。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DENY_TOML="${DENY_TOML:-$ROOT/deny.toml}"

if [[ ! -f "$DENY_TOML" ]]; then
  echo "error: deny.toml not found: $DENY_TOML" >&2
  exit 1
fi

# 仅解析 ignore 数组中的引号条目，避免把注释里的提及误收入 ignore
mapfile -t IGNORE_IDS < <(
  grep -oE '"RUSTSEC-[0-9]{4}-[0-9]+"' "$DENY_TOML" \
    | tr -d '"' \
    | sort -u
)

print_ignores() {
  if [[ ${#IGNORE_IDS[@]} -eq 0 ]]; then
    echo "(none)"
    return
  fi
  printf '%s\n' "${IGNORE_IDS[@]}"
}

if [[ "${1:-}" == "--print-ignores" || "${1:-}" == "--dry-parse" ]]; then
  print_ignores
  exit 0
fi

AUDIT_ARGS=()
for id in "${IGNORE_IDS[@]}"; do
  AUDIT_ARGS+=(--ignore "$id")
done

echo "cargo audit ignores (from deny.toml): ${IGNORE_IDS[*]:-(none)}"
cargo audit "${AUDIT_ARGS[@]}"

# 默认同时跑 cargo deny；独立 cargo-audit job 可设 AUDIT_WITH_DENY=0 跳过
if [[ "${AUDIT_WITH_DENY:-1}" == "1" ]]; then
  cargo deny check
fi
