#!/usr/bin/env bash
# SEC-013 产物签名验证脚本(配置检查,RED 测试)
#
# 由于 CI 配置无法用 `cargo test` 测,本脚本以静态检查方式验证
# release.yml 与 tauri.conf.json 是否已加入产物签名相关配置。
#
# 当前仓库未加签名配置,脚本应失败(RED),Implement Agent 加入
# Tauri updater ed25519 + sigstore cosign keyless 后转为 GREEN。
#
# 用法:
#   bash .github/scripts/verify-signature-config.sh
#
# 退出码:
#   0 = 全部检查通过(签名配置已就位)
#   1 = 至少一项检查失败(签名配置缺失)

set -euo pipefail

# 切到仓库根(脚本可在任意目录调用)
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

errors=0
pass_list=()
fail_list=()

check() {
  # check "检查项名称" "期望出现的文件路径(相对仓库根)" "grep 模式"
  local name="$1"
  local file="$2"
  local pattern="$3"
  if [ ! -f "$file" ]; then
    fail_list+=("$name: 文件不存在 $file")
    errors=$((errors + 1))
    return
  fi
  if grep -Eq -- "$pattern" "$file"; then
    pass_list+=("$name")
  else
    fail_list+=("$name: 在 $file 中未匹配 /$pattern/")
    errors=$((errors + 1))
  fi
}

check_jq() {
  # check_jq "检查项名称" "json 文件路径" "jq 查询表达式" "说明"
  local name="$1"
  local file="$2"
  local expr="$3"
  if [ ! -f "$file" ]; then
    fail_list+=("$name: 文件不存在 $file")
    errors=$((errors + 1))
    return
  fi
  local val
  if command -v jq >/dev/null 2>&1; then
    if ! val=$(jq -r "$expr" "$file" 2>/dev/null) || [ -z "$val" ] || [ "$val" = "null" ]; then
      fail_list+=("$name: jq 查询 $expr 在 $file 未命中")
      errors=$((errors + 1))
    else
      pass_list+=("$name ($val)")
    fi
  else
    # jq 不可用:回退到 grep 非空字段检查(粗粒度,仅看字段名存在)
    # 提取表达式末段 key 作为 grep 模式,例如 .plugins.updater.pubkey -> pubkey
    local key
    key="$(printf '%s' "$expr" | sed -E 's/^.*\.([a-zA-Z0-9_]+)$/\1/')"
    if grep -Eq -- "\"$key\"" "$file"; then
      pass_list+=("$name (grep 回退,字段 $key 存在)")
    else
      fail_list+=("$name: jq 不可用且 grep 未在 $file 命中字段 $key")
      errors=$((errors + 1))
    fi
  fi
}

release_yml=".github/workflows/release.yml"
tauri_json="crates/tachyon-app/tauri.conf.json"

echo "=== SEC-013 产物签名配置检查 ==="
echo "仓库根: $repo_root"
echo ""

# ── A. Tauri updater ed25519 ──────────────────────────────
# 1) tauri.conf.json 含 plugins.updater.pubkey 字段
check_jq \
  "A1 tauri.conf.json 含 updater.pubkey" \
  "$tauri_json" \
  '.plugins.updater.pubkey' \
  "Tauri updater ed25519 公钥"

# 2) tauri.conf.json 含 plugins.updater.endpoints 数组(非空)
check_jq \
  "A2 tauri.conf.json 含 updater.endpoints" \
  "$tauri_json" \
  '.plugins.updater.endpoints[0]' \
  "Tauri updater 端点 URL"

# 3) release.yml tauri-action step 注入 TAURI_SIGNING_PRIVATE_KEY
check \
  "A3 release.yml 含 TAURI_SIGNING_PRIVATE_KEY env" \
  "$release_yml" \
  "TAURI_SIGNING_PRIVATE_KEY"

# 4) release.yml 注入 TAURI_SIGNING_PRIVATE_KEY_PASSWORD(或显式缺省为空)
check \
  "A4 release.yml 含 TAURI_SIGNING_PRIVATE_KEY_PASSWORD env" \
  "$release_yml" \
  "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"

# 5) 产物 .sig 旁路文件生成(Tauri action 自动产生,签名配置生效后产物目录含 .sig)
#    这里仅校验配置端,.sig 文件级校验交给 install-smoke / publish-release
#    配置层不直接 grep .sig(易误伤),故 A5 略

# ── B. 签名产物存在性(配置层留空,文件层由 install-smoke 覆盖) ──

# ── C. sigstore cosign keyless ─────────────────────────────
# 6) publish-release job 声明 id-token: write(OIDC keyless 前提)
check \
  "C1 release.yml 含 id-token: write 权限" \
  "$release_yml" \
  "id-token:[[:space:]]*write"

# 7) 使用 sigstore/cosign-installer setup action
check \
  "C2 release.yml 引用 sigstore/cosign-installer" \
  "$release_yml" \
  "sigstore/cosign-installer"

# 8) 调用 cosign sign-blob --yes --bundle <file>.bundle <file>
check \
  "C3 release.yml 含 cosign sign-blob --bundle 调用" \
  "$release_yml" \
  "cosign[[:space:]]+sign-blob.*--bundle"

# ── 输出 ───────────────────────────────────────────────────
echo "--- 通过 ---"
if [ ${#pass_list[@]} -eq 0 ]; then
  echo "  (无)"
fi
for item in "${pass_list[@]}"; do
  echo "  [OK] $item"
done

echo ""
echo "--- 失败 ---"
if [ ${#fail_list[@]} -eq 0 ]; then
  echo "  (无)"
fi
for item in "${fail_list[@]}"; do
  echo "  [FAIL] $item"
done

echo ""
if [ "$errors" -eq 0 ]; then
  echo "SEC-013 产物签名配置检查: 全部通过"
  exit 0
else
  echo "SEC-013 产物签名配置检查: $errors 项失败(RED)"
  exit 1
fi
