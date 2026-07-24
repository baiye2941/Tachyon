#!/usr/bin/env bash
# SSOT: Profile-Guided Optimization 工作流(审计 P-02)
#
# PGO 行业经验可带来 5-15% 吞吐提升,本脚本提供可复现的 generate/use 两阶段。
# 默认仅覆盖 tachyon-engine 热路径 bench(e2e_download),避免全量 bench 过久。
#
# 用法:
#   bash scripts/ci/pgo.sh generate   # 插桩构建 + 跑 bench 产出 profile
#   bash scripts/ci/pgo.sh use        # 用 profile 优化构建 release
#   bash scripts/ci/pgo.sh clean      # 清理 profile 与 pgo target
#
# 注意:
# - 需要 clang/llvm-profdata(Linux/macOS 推荐;Windows 需 MSVC PGO 另配,本脚本不覆盖)
# - 产出目录默认 target/pgo/(可用 TACHYON_PGO_DIR 覆盖)
# - 不进 CI 关键路径(与 mutants/bench 同:仅 main 采样或本地手动)
set -euo pipefail

MODE="${1:-}"
PGO_DIR="${TACHYON_PGO_DIR:-target/pgo}"
PROFDATA="${PGO_DIR}/merged.profdata"
RAW_DIR="${PGO_DIR}/raw"

die() { echo "error: $*" >&2; exit 1; }

# 解析 llvm-profdata:PATH → 版本后缀 → rustup llvm-tools(Windows 常见)
resolve_llvm_profdata() {
  if command -v llvm-profdata >/dev/null 2>&1; then
    command -v llvm-profdata
    return
  fi
  if command -v llvm-profdata-18 >/dev/null 2>&1; then
    command -v llvm-profdata-18
    return
  fi
  if command -v llvm-profdata-17 >/dev/null 2>&1; then
    command -v llvm-profdata-17
    return
  fi
  local sysroot
  sysroot="$(rustc --print sysroot 2>/dev/null || true)"
  if [[ -n "$sysroot" ]]; then
    local cand
    # Unix: lib/rustlib/<triple>/bin/llvm-profdata
    # Windows: lib/rustlib/<triple>/bin/llvm-profdata.exe
    while IFS= read -r cand; do
      if [[ -x "$cand" ]]; then
        printf '%s
' "$cand"
        return
      fi
    done < <(find "$sysroot/lib/rustlib" -name 'llvm-profdata*' 2>/dev/null | head -5)
  fi
  return 1
}

need_llvm() {
  LLVM_PROFDATA_BIN="$(resolve_llvm_profdata)" || die "需要 llvm-profdata(rustup component add llvm-tools-preview 或系统 LLVM)"
  export LLVM_PROFDATA_BIN
  echo "using llvm-profdata: $LLVM_PROFDATA_BIN"
}

llvm_profdata() {
  need_llvm
  "$LLVM_PROFDATA_BIN" "$@"
}

case "${MODE}" in
  generate)
    need_llvm
    mkdir -p "${RAW_DIR}"
    rm -rf "${RAW_DIR:?}"/* "${PROFDATA}" 2>/dev/null || true
    echo "==> PGO generate: 插桩构建 + e2e_download 采样"
    # rustc 插桩:profile-generate 写出 .profraw
    export RUSTFLAGS="-Cprofile-generate=${RAW_DIR} ${RUSTFLAGS:-}"
    cargo build --release -p tachyon-engine --all-features
    # 用 ci 模式 bench 快速产出代表性 profile
    TACHYON_BENCH_MODE=ci cargo bench --bench e2e_download -- --sample-size 10 --warm-up-time 1 --measurement-time 2
    # 合并
    mapfile -t RAWS < <(find "${RAW_DIR}" -name '*.profraw' 2>/dev/null || true)
    if [[ ${#RAWS[@]} -eq 0 ]]; then
      die "未找到 .profraw,确认 RUSTFLAGS 插桩生效且 bench 跑过"
    fi
    llvm_profdata merge -o "${PROFDATA}" "${RAWS[@]}"
    echo "==> 已写出 ${PROFDATA}"
    ;;
  use)
    [[ -f "${PROFDATA}" ]] || die "缺少 ${PROFDATA},先运行: bash scripts/ci/pgo.sh generate"
    echo "==> PGO use: 用 profile 优化 release 构建"
    export RUSTFLAGS="-Cprofile-use=${PROFDATA} -Cllvm-args=-pgo-warn-missing-function ${RUSTFLAGS:-}"
    cargo build --release -p tachyon-engine --all-features
    echo "==> PGO 优化构建完成"
    ;;
  clean)
    rm -rf "${PGO_DIR}"
    echo "==> 已清理 ${PGO_DIR}"
    ;;
  *)
    cat <<'EOF'
用法: bash scripts/ci/pgo.sh {generate|use|clean}

  generate  插桩构建 + 跑 e2e_download bench,合并为 target/pgo/merged.profdata
  use       用 merged.profdata 做 profile-use release 构建
  clean     删除 target/pgo

环境变量:
  TACHYON_PGO_DIR  profile 目录(默认 target/pgo)
EOF
    exit 1
    ;;
esac
