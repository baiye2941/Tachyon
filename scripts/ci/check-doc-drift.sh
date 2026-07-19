#!/usr/bin/env bash
# 文档/工作流漂移检测：禁止假同源命令与内联门禁逻辑回流 YAML。
#
# 用法:
#   bash scripts/ci/check-doc-drift.sh
#
# 退出码:
#   0 无漂移
#   1 发现禁止模式
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

errors=0

fail() {
  echo "::error::$1" >&2
  errors=$((errors + 1))
}

echo "=== 文档漂移检测 ==="

# 1) 禁止跟踪文档把合计 fail-under-lines 当 CI 门禁广告
#    （允许历史 plan/aegis 归档，仅扫 README + docs 下非 plans/aegis 路径）
while IFS= read -r -d '' f; do
  if grep -nE -- '--fail-under-lines[[:space:]]+90|fail-under-lines 90' "$f" >/dev/null 2>&1; then
    # 若同一文件明确写「禁止」则放过
    if grep -nE '禁止.*fail-under-lines|不得.*fail-under-lines|废弃.*fail-under-lines' "$f" >/dev/null 2>&1; then
      continue
    fi
    fail "文档仍广告 fail-under-lines 门禁: $f（应改为 scripts/ci/coverage.sh / regions 逐 crate）"
    grep -nE -- '--fail-under-lines[[:space:]]+90|fail-under-lines 90' "$f" | head -5 || true
  fi
done < <(find README.md docs -type f \( -name '*.md' -o -name '*.mdx' \) \
  ! -path 'docs/superpowers/plans/*' \
  ! -path 'docs/aegis/*' \
  ! -path 'docs/sdd/*' \
  -print0 2>/dev/null)

# 2) workflow 禁止内联 cargo llvm-cov / cargo miri test 业务逻辑（必须走 scripts/ci）
for wf in .github/workflows/ci.yml .github/workflows/release.yml; do
  [[ -f "$wf" ]] || continue
  if grep -nE 'cargo[[:space:]]+llvm-cov' "$wf" >/dev/null 2>&1; then
    fail "$wf 内联 cargo llvm-cov（应 bash scripts/ci/coverage.sh）"
    grep -nE 'cargo[[:space:]]+llvm-cov' "$wf" | head -5 || true
  fi
  if grep -nE 'cargo[[:space:]]+miri[[:space:]]+test' "$wf" >/dev/null 2>&1; then
    fail "$wf 内联 cargo miri test（应 bash scripts/ci/miri.sh）"
    grep -nE 'cargo[[:space:]]+miri[[:space:]]+test' "$wf" | head -5 || true
  fi
  if grep -nE 'cargo[[:space:]]+audit[[:space:]]+--ignore' "$wf" >/dev/null 2>&1; then
    fail "$wf 手写 cargo audit --ignore（应 bash scripts/ci/audit.sh 从 deny.toml 解析）"
    grep -nE 'cargo[[:space:]]+audit[[:space:]]+--ignore' "$wf" | head -5 || true
  fi
done

# 3) 禁止 ci-pass 再写 needs.*.result（GHA 假绿）——只匹配表达式，忽略注释
if grep -nE '\$\{\{[[:space:]]*needs\.\*\.result' .github/workflows/ci.yml >/dev/null 2>&1; then
  fail "ci.yml 使用 needs.*.result（不会展开为各 job 结果，会导致假绿）"
  grep -nE '\$\{\{[[:space:]]*needs\.\*\.result' .github/workflows/ci.yml || true
fi

if [[ "$errors" -ne 0 ]]; then
  echo "文档/工作流漂移检测失败: $errors 项" >&2
  exit 1
fi

echo "文档/工作流漂移检测通过"
exit 0
