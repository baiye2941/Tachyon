#!/usr/bin/env bash
# SSOT: 版本一致性检查（Release version-check 门禁）
# 模式:
#   files — 仅校验 Cargo.toml / tauri.conf.json / frontend/package.json 三者互相同
#   tag   — 在 files 基础上再校验 GITHUB_REF_NAME 去 v 前缀 == cargo 版本
#   auto  — GITHUB_REF 为 refs/tags/v* 时等同 tag，否则等同 files
# 用法: bash scripts/ci/version-check.sh [tag|files|auto]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

mode="${1:-auto}" # tag | files | auto

case "$mode" in
  tag|files|auto) ;;
  *)
    echo "::error::未知 mode='$mode'（期望 tag|files|auto）" >&2
    exit 2
    ;;
esac

cargo_v=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\([^"]*\)".*/\1/')
tauri_v=$(grep -m1 '"version"' crates/tachyon-app/tauri.conf.json | sed 's/.*"\([^"]*\)".*/\1/')
fe_v=$(grep -m1 '"version"' frontend/package.json | sed 's/.*"\([^"]*\)".*/\1/')

echo "cargo=$cargo_v tauri=$tauri_v frontend=$fe_v mode=$mode"

[[ -n "$cargo_v" && -n "$tauri_v" && -n "$fe_v" ]] || {
  echo "::error::版本字段解析失败 cargo='$cargo_v' tauri='$tauri_v' frontend='$fe_v'"
  exit 1
}

[[ "$cargo_v" == "$tauri_v" && "$cargo_v" == "$fe_v" ]] || {
  echo "::error::版本互不一致 cargo=$cargo_v tauri=$tauri_v frontend=$fe_v"
  exit 1
}

if [[ "$mode" == "tag" || ( "$mode" == "auto" && "${GITHUB_REF:-}" == refs/tags/v* ) ]]; then
  # tag 模式优先用 GITHUB_REF_NAME（GitHub 提供的短名），回退从 GITHUB_REF 剥离
  if [[ -n "${GITHUB_REF_NAME:-}" ]]; then
    tag_v="${GITHUB_REF_NAME#v}"
  else
    tag_v="${GITHUB_REF#refs/tags/v}"
  fi
  [[ "$tag_v" == "$cargo_v" ]] || {
    echo "::error::tag=$tag_v != cargo=$cargo_v"
    exit 1
  }
  echo "version ok: $cargo_v (mode=$mode, tag matched)"
else
  echo "version ok: $cargo_v (mode=$mode, files-only)"
fi
