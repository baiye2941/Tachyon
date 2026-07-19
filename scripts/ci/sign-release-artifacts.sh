#!/usr/bin/env bash
# SSOT: 发布产物递归 SHA256 + cosign 签名与硬断言
# 与 install-smoke 同一套扩展：msi / deb / dmg / AppImage
#
# 用法:
#   bash scripts/ci/sign-release-artifacts.sh [DIST_DIR]
#
# 环境变量:
#   DIST_DIR         产物目录（默认 dist；也可用位置参数）
#   SKIP_COSIGN      设为 1 时跳过 cosign，写占位 .bundle（本地断言测试）
#   RECONCILE_TAG    若设置，从 GitHub Release 下载资产并与本地安装包做字节对账
#   RECONCILE_DIR    对账下载目录（默认 release-assets）
#   GITHUB_REPOSITORY / GH_TOKEN  对账时 gh 需要
#
# 退出码:
#   0 成功（至少 1 个产物，sha256/bundle 数量与产物一致，对账通过）
#   1 无产物 / 签名失败 / 数量不一致 / 对账失败
#   2 参数/环境错误
set -euo pipefail

DIST_DIR="${1:-${DIST_DIR:-dist}}"

if [[ ! -d "$DIST_DIR" ]]; then
  echo "::error::产物目录不存在: $DIST_DIR" >&2
  exit 1
fi

echo "=== 产物目录树 ==="
find "$DIST_DIR" -type f -ls 2>/dev/null || ls -laR "$DIST_DIR"

# 清理旧 sidecar，避免计数污染（仅本脚本生成的扩展）
find "$DIST_DIR" -type f \( -name '*.sha256' -o -name '*.bundle' \) -delete 2>/dev/null || true

# 与 install-smoke 同一套递归找包，且要求非零大小（-size +0c）
mapfile -t FILES < <(
  find "$DIST_DIR" -type f -size +0c \( \
    -name '*.msi' -o -name '*.deb' -o -name '*.dmg' -o -name '*.AppImage' \
  \) | LC_ALL=C sort
)

if [[ ${#FILES[@]} -lt 1 ]]; then
  # 区分：完全无包 vs 仅有零字节包
  mapfile -t ZERO_OR_ANY < <(
    find "$DIST_DIR" -type f \( \
      -name '*.msi' -o -name '*.deb' -o -name '*.dmg' -o -name '*.AppImage' \
    \) | LC_ALL=C sort
  )
  if [[ ${#ZERO_OR_ANY[@]} -ge 1 ]]; then
    echo "::error::仅发现零字节安装包（${ZERO_OR_ANY[*]}），拒绝签名（与 install-smoke -size +0c 对齐）" >&2
  else
    echo "::error::无产物可签名（在 $DIST_DIR 下未找到 msi/deb/dmg/AppImage）" >&2
  fi
  exit 1
fi

echo "找到 ${#FILES[@]} 个安装包:"
printf '  %s\n' "${FILES[@]}"

for f in "${FILES[@]}"; do
  # 校验文件内容用 basename，便于用户下载后 sha256sum -c
  (
    cd "$(dirname "$f")"
    sha256sum "$(basename "$f")"
  ) | tee "$f.sha256"

  if [[ "${SKIP_COSIGN:-0}" == "1" ]]; then
    # 本地/单测：占位 bundle，只验证递归+硬断言路径
    : >"$f.bundle"
    echo "SKIP_COSIGN=1: 写入占位 bundle $f.bundle"
  else
    cosign sign-blob --yes --bundle "$f.bundle" "$f"
    echo "已签名: $f -> $f.bundle"
  fi
done

# 硬断言：sidecar 数量必须与安装包一一对应
# 用 -type f 且限定在 DIST_DIR，避免误计
sha_count=$(find "$DIST_DIR" -type f -name '*.sha256' | wc -l | tr -d '[:space:]')
bundle_count=$(find "$DIST_DIR" -type f -name '*.bundle' | wc -l | tr -d '[:space:]')
file_count=${#FILES[@]}

if [[ "$sha_count" -ne "$file_count" ]]; then
  echo "::error::sha256 数量($sha_count) != 产物数量($file_count)" >&2
  exit 1
fi
if [[ "$bundle_count" -ne "$file_count" ]]; then
  echo "::error::bundle 数量($bundle_count) != 产物数量($file_count)" >&2
  exit 1
fi

echo "签名硬断言通过: artifacts=$file_count sha256=$sha_count bundle=$bundle_count"

# 可选：对 Release 资产做字节级对账（避免 artifact 布局与用户下载物不一致）
if [[ -n "${RECONCILE_TAG:-}" ]]; then
  if [[ -z "${GITHUB_REPOSITORY:-}" ]]; then
    echo "::error::RECONCILE_TAG 已设置但缺少 GITHUB_REPOSITORY" >&2
    exit 2
  fi
  RECONCILE_DIR="${RECONCILE_DIR:-release-assets}"
  rm -rf "$RECONCILE_DIR"
  mkdir -p "$RECONCILE_DIR"

  echo "=== 下载 Release 资产做对账: tag=$RECONCILE_TAG ==="
  gh release download "$RECONCILE_TAG" \
    --dir "$RECONCILE_DIR" \
    --pattern '*' \
    --repo "$GITHUB_REPOSITORY"

  for f in "${FILES[@]}"; do
    base=$(basename "$f")
    mapfile -t matches < <(find "$RECONCILE_DIR" -type f -name "$base" | LC_ALL=C sort)
    if [[ ${#matches[@]} -eq 0 ]]; then
      echo "::error::Release 资产缺失: $base（本地 artifact 有，Release 无）" >&2
      exit 1
    fi
    if [[ ${#matches[@]} -gt 1 ]]; then
      echo "::error::Release 资产 basename 多重匹配: $base → ${matches[*]}（拒绝静默取 matches[0]）" >&2
      exit 1
    fi
    local_hash=$(sha256sum "$f" | awk '{print $1}')
    remote_hash=$(sha256sum "${matches[0]}" | awk '{print $1}')
    if [[ "$local_hash" != "$remote_hash" ]]; then
      echo "::error::字节对账失败: $base local=$local_hash release=$remote_hash" >&2
      exit 1
    fi
    echo "对账通过: $base ($local_hash)"
  done
  echo "Release 资产对账全部通过 (${#FILES[@]} 个)"
fi
