#!/usr/bin/env bash
# SEC-013 产物签名验证脚本(配置检查)
#
# 由于 CI 配置无法用 `cargo test` 测,本脚本以静态检查方式验证
# release.yml 与 tauri.conf.json 是否已加入产物签名相关配置。
#
# 用法:
#   bash .github/scripts/verify-signature-config.sh
#
# 退出码:
#   0 = 全部检查通过(签名配置已就位)
#   1 = 至少一项检查失败(签名配置缺失/仍为 PLACEHOLDER)
#
# 硬化规则:
#   - updater.pubkey 含 PLACEHOLDER 必须 FAIL（禁止假绿）
#   - endpoints 必须为非空 URL 数组（python/jq 可靠解析，禁止仅看字段名）

set -euo pipefail

# 切到仓库根(脚本可在任意目录调用)
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

errors=0
pass_list=()
fail_list=()

pass() {
  pass_list+=("$1")
}

fail() {
  fail_list+=("$1")
  errors=$((errors + 1))
}

check() {
  # check "检查项名称" "期望出现的文件路径(相对仓库根)" "grep 模式"
  local name="$1"
  local file="$2"
  local pattern="$3"
  if [ ! -f "$file" ]; then
    fail "$name: 文件不存在 $file"
    return
  fi
  if grep -Eq -- "$pattern" "$file"; then
    pass "$name"
  else
    fail "$name: 在 $file 中未匹配 /$pattern/"
  fi
}

# 用 python 可靠解析 JSON（Windows/Linux 均常见；不依赖 jq）
json_get() {
  # json_get <file> <python-expr-on-data>
  local file="$1"
  local expr="$2"
  if command -v python3 >/dev/null 2>&1; then
    PY=python3
  elif command -v python >/dev/null 2>&1; then
    PY=python
  else
    return 2
  fi
  "$PY" - "$file" <<PY
import json, sys
path = sys.argv[1]
with open(path, encoding="utf-8") as f:
    data = json.load(f)
val = $expr
if val is None:
    sys.exit(3)
if isinstance(val, (list, dict)):
    import json as _j
    print(_j.dumps(val, ensure_ascii=False))
else:
    print(val)
PY
}

release_yml=".github/workflows/release.yml"
tauri_json="crates/tachyon-app/tauri.conf.json"

echo "=== SEC-013 产物签名配置检查 ==="
echo "仓库根: $repo_root"
echo ""

# ── A. Tauri updater ed25519 ──────────────────────────────
# A1: pubkey 存在且非 PLACEHOLDER
a1_name="A1 tauri.conf.json updater.pubkey 已配置且非 PLACEHOLDER"
if [ ! -f "$tauri_json" ]; then
  fail "$a1_name: 文件不存在 $tauri_json"
else
  if pubkey_val="$(json_get "$tauri_json" "data.get('plugins', {}).get('updater', {}).get('pubkey')")"; then
    if [ -z "$pubkey_val" ] || [ "$pubkey_val" = "null" ]; then
      fail "$a1_name: pubkey 为空"
    elif printf '%s' "$pubkey_val" | grep -Eqi 'PLACEHOLDER'; then
      fail "$a1_name: pubkey 仍为 PLACEHOLDER（禁止发布假绿）: $pubkey_val"
    else
      # 不打印完整公钥，仅提示长度
      pass "$a1_name (len=${#pubkey_val})"
    fi
  else
    fail "$a1_name: 无法解析 JSON（需要 python/python3）"
  fi
fi

# A2: endpoints 非空且首项为 http(s) URL
a2_name="A2 tauri.conf.json updater.endpoints 非空 URL"
if [ ! -f "$tauri_json" ]; then
  fail "$a2_name: 文件不存在 $tauri_json"
else
  if endpoints_json="$(json_get "$tauri_json" "data.get('plugins', {}).get('updater', {}).get('endpoints')")"; then
    if first_ep="$(json_get "$tauri_json" "(data.get('plugins', {}).get('updater', {}).get('endpoints') or [None])[0]")"; then
      if [ -z "$first_ep" ] || [ "$first_ep" = "null" ]; then
        fail "$a2_name: endpoints 为空"
      elif printf '%s' "$first_ep" | grep -Eq '^https?://'; then
        pass "$a2_name ($first_ep)"
      else
        fail "$a2_name: 首项不是 http(s) URL: $first_ep"
      fi
    else
      fail "$a2_name: endpoints 为空或不可解析 ($endpoints_json)"
    fi
  else
    fail "$a2_name: 无法解析 endpoints"
  fi
fi

# A3/A4: release.yml 注入签名私钥相关 env
check \
  "A3 release.yml 含 TAURI_SIGNING_PRIVATE_KEY env" \
  "$release_yml" \
  "TAURI_SIGNING_PRIVATE_KEY"

check \
  "A4 release.yml 含 TAURI_SIGNING_PRIVATE_KEY_PASSWORD env" \
  "$release_yml" \
  "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"

# ── C. sigstore cosign keyless ─────────────────────────────
check \
  "C1 release.yml 含 id-token: write 权限" \
  "$release_yml" \
  "id-token:[[:space:]]*write"

check \
  "C2 release.yml 引用 sigstore/cosign-installer" \
  "$release_yml" \
  "sigstore/cosign-installer"

# C3: cosign sign-blob --bundle（内联或 SSOT 脚本）
sign_script="scripts/ci/sign-release-artifacts.sh"
c3_name="C3 cosign sign-blob --bundle（release.yml 内联或 SSOT 脚本）"
c3_ok=0
if [ -f "$release_yml" ] && grep -Eq -- "cosign[[:space:]]+sign-blob.*--bundle" "$release_yml"; then
  c3_ok=1
elif [ -f "$release_yml" ] && [ -f "$sign_script" ] \
  && grep -Eq -- "scripts/ci/sign-release-artifacts\\.sh" "$release_yml" \
  && grep -Eq -- "cosign[[:space:]]+sign-blob.*--bundle" "$sign_script"; then
  c3_ok=1
fi
if [ "$c3_ok" -eq 1 ]; then
  pass "$c3_name"
else
  fail "$c3_name: release.yml 未内联 cosign sign-blob --bundle，且未引用含该调用的 $sign_script"
fi

# ── 输出 ───────────────────────────────────────────────────
# 注意: macOS/bash 在 set -u 下对空数组 "${arr[@]}" 会 unbound variable
echo "--- 通过 ---"
if [ "${#pass_list[@]}" -eq 0 ]; then
  echo "  (无)"
else
  for item in "${pass_list[@]}"; do
    echo "  [OK] $item"
  done
fi

echo ""
echo "--- 失败 ---"
if [ "${#fail_list[@]}" -eq 0 ]; then
  echo "  (无)"
else
  for item in "${fail_list[@]}"; do
    echo "  [FAIL] $item"
  done
fi

echo ""
if [ "$errors" -eq 0 ]; then
  echo "SEC-013 产物签名配置检查: 全部通过"
  exit 0
else
  echo "SEC-013 产物签名配置检查: $errors 项失败(RED)"
  exit 1
fi
