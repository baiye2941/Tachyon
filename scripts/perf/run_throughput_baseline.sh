#!/usr/bin/env bash
# 可重复吞吐基线场景矩阵编排(Linux/macOS)
# 用法:
#   bash scripts/perf/run_throughput_baseline.sh --quick
#   bash scripts/perf/run_throughput_baseline.sh --size 512MiB --compare-aria2
#   bash scripts/perf/run_throughput_baseline.sh --url https://... --mirror https://mirror/...
set -euo pipefail

QUICK=0
SIZE="64MiB"
RUNS=3
CONCURRENCY=16
COMPARE_ARIA2=0
PRIMARY_URL=""
MIRROR_URL=""
OUT_DIR="target/perf-baseline"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --quick) QUICK=1; shift ;;
    --size) SIZE="$2"; shift 2 ;;
    --runs) RUNS="$2"; shift 2 ;;
    --concurrency) CONCURRENCY="$2"; shift 2 ;;
    --compare-aria2) COMPARE_ARIA2=1; shift ;;
    --url) PRIMARY_URL="$2"; shift 2 ;;
    --mirror) MIRROR_URL="$2"; shift 2 ;;
    --out-dir) OUT_DIR="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,7p' "$0"
      exit 0
      ;;
    *) echo "未知参数: $1"; exit 2 ;;
  esac
done

mkdir -p "$OUT_DIR"
export RUST_LOG="${RUST_LOG:-warn}"

run_one() {
  local name="$1"; shift
  local out="$OUT_DIR/${name}.json"
  echo "=== scenario: $name ==="
  local extra=( "$@" --out "$out" )
  if [[ "$COMPARE_ARIA2" -eq 1 ]]; then
    extra+=(--compare-aria2)
  fi
  cargo bench --bench throughput_baseline -- "${extra[@]}"
}

if [[ -n "$PRIMARY_URL" ]]; then
  args=(--url "$PRIMARY_URL" --runs "$RUNS" --concurrency "$CONCURRENCY")
  if [[ -n "$MIRROR_URL" ]]; then
    args+=(--mirror "$MIRROR_URL")
  fi
  run_one external_primary "${args[@]}"
  echo "done. results under $OUT_DIR"
  exit 0
fi

run_one loopback_unthrottled --size "$SIZE" --rtt-ms 0 --bps 0 --runs "$RUNS" --concurrency "$CONCURRENCY"

if [[ "$QUICK" -eq 0 ]]; then
  run_one rtt50 --size "$SIZE" --rtt-ms 50 --bps 0 --runs "$RUNS" --concurrency "$CONCURRENCY"
  run_one rtt100 --size "$SIZE" --rtt-ms 100 --bps 0 --runs "$RUNS" --concurrency "$CONCURRENCY"
  run_one rtt200 --size "$SIZE" --rtt-ms 200 --bps 0 --runs "$RUNS" --concurrency "$CONCURRENCY"
  run_one cap_100Mbps --size "$SIZE" --rtt-ms 0 --bps 12.5M --runs "$RUNS" --concurrency "$CONCURRENCY"
  run_one cap_1Gbps_rtt50 --size "$SIZE" --rtt-ms 50 --bps 125M --runs "$RUNS" --concurrency "$CONCURRENCY"
fi

echo
echo "全部完成. JSON: $OUT_DIR"
echo "指标字段: goodput_bps / aligned_write_* / rebalance_count / peak_active_requests"
echo "CPU%/磁盘队列: 请用 top/iostat/perf 外挂采样(本 harness 不伪造)"
echo "丢包 0/1/2%: tc netem 示例见 docs/sdd/throughput-baseline.md"
echo "文档: docs/sdd/throughput-baseline.md"
