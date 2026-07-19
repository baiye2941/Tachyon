#!/usr/bin/env python3
"""从 cargo-llvm-cov --json 输出按 crate 聚合 regions 覆盖率并断言阈值。

用法:
  python3 scripts/ci/coverage_assert.py coverage.json \\
    --threshold 90 \\
    --crates tachyon-core,tachyon-engine,... \\
    --ignore-regex '(test_harness|iocp|winio|iouring)'

退出码:
  0 全部达标
  1 有 crate 低于阈值或缺少数据
  2 参数/JSON 错误
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from collections import defaultdict
from pathlib import Path, PurePosixPath


def normalize_path(p: str) -> str:
    return p.replace("\\", "/")


def crate_of(filename: str, crates: list[str]) -> str | None:
    """路径中匹配 crates/<name>/ 或 /<name>/src/ 的最长 crate 名。"""
    path = normalize_path(filename)
    best: str | None = None
    for crate in crates:
        markers = (
            f"/crates/{crate}/",
            f"/{crate}/src/",
            f"/{crate}/tests/",
            f"/{crate}/benches/",
        )
        if any(m in path for m in markers):
            if best is None or len(crate) > len(best):
                best = crate
    return best


def load_files(data: dict) -> list[dict]:
    files: list[dict] = []
    for entry in data.get("data", []):
        files.extend(entry.get("files") or [])
    # 兼容顶层 files
    if not files and "files" in data:
        files = data["files"]
    return files


def aggregate(
    files: list[dict],
    crates: list[str],
    ignore_re: re.Pattern[str] | None,
) -> dict[str, dict[str, float]]:
    """返回 crate -> {covered, count, percent}。"""
    acc: dict[str, dict[str, float]] = {
        c: {"covered": 0.0, "count": 0.0} for c in crates
    }
    for f in files:
        filename = f.get("filename") or f.get("file") or ""
        if not filename:
            continue
        path = normalize_path(filename)
        if ignore_re and ignore_re.search(path):
            continue
        crate = crate_of(path, crates)
        if crate is None:
            continue
        summary = f.get("summary") or {}
        regions = summary.get("regions") or {}
        # llvm-cov export: count/covered 或 covered/count
        count = float(regions.get("count") or regions.get("count") or 0)
        covered = float(regions.get("covered") or 0)
        # 有的导出只有 percent
        if count == 0 and "percent" in regions:
            # 无法聚合，跳过该文件
            continue
        acc[crate]["count"] += count
        acc[crate]["covered"] += covered
    for crate, v in acc.items():
        c = v["count"]
        v["percent"] = (100.0 * v["covered"] / c) if c > 0 else 0.0
    return acc


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="逐 crate regions 覆盖率断言")
    parser.add_argument("json_path", type=Path, help="cargo llvm-cov --json 输出文件")
    parser.add_argument(
        "--threshold",
        type=float,
        default=90.0,
        help="regions 覆盖率下限百分比（默认 90）",
    )
    parser.add_argument(
        "--crates",
        required=True,
        help="逗号分隔 crate 列表",
    )
    parser.add_argument(
        "--ignore-regex",
        default="",
        help="忽略路径的正则（与 cargo llvm-cov --ignore-filename-regex 对齐）",
    )
    args = parser.parse_args(argv)

    crates = [c.strip() for c in args.crates.split(",") if c.strip()]
    if not crates:
        print("::error::--crates 为空", file=sys.stderr)
        return 2

    if not args.json_path.is_file():
        print(f"::error::覆盖率 JSON 不存在: {args.json_path}", file=sys.stderr)
        return 2

    try:
        data = json.loads(args.json_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        print(f"::error::JSON 解析失败: {e}", file=sys.stderr)
        return 2

    ignore_re = re.compile(args.ignore_regex) if args.ignore_regex else None
    files = load_files(data)
    if not files:
        print("::error::JSON 中无 files 覆盖数据", file=sys.stderr)
        return 1

    stats = aggregate(files, crates, ignore_re)
    failed = False
    print("=== 逐 crate regions 覆盖率 ===")
    for crate in crates:
        s = stats[crate]
        pct = s["percent"]
        covered = int(s["covered"])
        count = int(s["count"])
        line = f"{crate}: regions {pct:.2f}% ({covered}/{count})"
        if count == 0:
            print(f"::error::{line} — 无覆盖数据")
            failed = True
        elif pct + 1e-9 < args.threshold:
            print(f"::error::{line} < {args.threshold}%")
            failed = True
        else:
            print(f"OK  {line}")

    if failed:
        print(f"::error::覆盖率门禁失败（阈值 regions >= {args.threshold}%）", file=sys.stderr)
        return 1
    print(f"覆盖率门禁通过: {len(crates)} crates regions >= {args.threshold}%")
    return 0


if __name__ == "__main__":
    sys.exit(main())
