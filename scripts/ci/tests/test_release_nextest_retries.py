#!/usr/bin/env python3
"""CI/release nextest retries 契约：禁止 workflow 覆盖 SSOT retries=0。

审计 E-04：.config/nextest.toml profile.default.retries = 0。
CI 与 release 不得再传 --retries N（N>0），否则会掩盖真实回归。
"""
from __future__ import annotations

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
RELEASE_YML = ROOT / ".github" / "workflows" / "release.yml"
CI_YML = ROOT / ".github" / "workflows" / "ci.yml"
NEXTEST_TOML = ROOT / ".config" / "nextest.toml"

# 匹配显式非零重试：--retries 2 / --retries=2 / --retries  3
NONZERO_RETRIES_RE = re.compile(
    r"--retries(?:\s+|=)(?:[1-9]\d*)\b",
    re.MULTILINE,
)


def _non_comment_lines(text: str) -> list[str]:
    """YAML/TOML 行级注释剔除（# 行首或行内简易剥离，足够扫 run: 命令）。"""
    out: list[str] = []
    for line in text.splitlines():
        stripped = line.lstrip()
        if stripped.startswith("#"):
            continue
        # 行内注释：仅当 # 不在引号内时粗切（workflow run 命令无复杂引号）
        if "#" in line:
            line = line.split("#", 1)[0]
        out.append(line)
    return out


def find_nonzero_retries(path: Path) -> list[str]:
    text = path.read_text(encoding="utf-8")
    hits: list[str] = []
    for i, line in enumerate(_non_comment_lines(text), start=1):
        if NONZERO_RETRIES_RE.search(line):
            hits.append(f"{path.name}:{i}: {line.strip()}")
    # 也扫原始全文，避免注释剥离误伤时漏检真实命令
    raw_hits = []
    for i, line in enumerate(text.splitlines(), start=1):
        stripped = line.lstrip()
        if stripped.startswith("#"):
            continue
        code = line.split("#", 1)[0] if "#" in line else line
        if NONZERO_RETRIES_RE.search(code):
            raw_hits.append(f"{path.name}:{i}: {line.rstrip()}")
    return raw_hits


class TestReleaseNextestRetries(unittest.TestCase):
    def test_nextest_toml_ssot_retries_zero(self) -> None:
        self.assertTrue(NEXTEST_TOML.is_file(), f"missing {NEXTEST_TOML}")
        text = NEXTEST_TOML.read_text(encoding="utf-8")
        self.assertRegex(
            text,
            re.compile(r"(?m)^\s*retries\s*=\s*0\s*$"),
            "nextest SSOT 必须 retries = 0（审计 E-04）",
        )
        self.assertIsNone(
            re.search(r"(?m)^\s*retries\s*=\s*[1-9]\d*\s*$", text),
            "nextest.toml 不得设置非零 retries",
        )

    def test_release_yml_forbids_nonzero_retries(self) -> None:
        self.assertTrue(RELEASE_YML.is_file(), f"missing {RELEASE_YML}")
        hits = find_nonzero_retries(RELEASE_YML)
        self.assertEqual(
            hits,
            [],
            "release.yml 禁止 --retries N（N>0）；应依赖 .config/nextest.toml retries=0。命中:\n"
            + "\n".join(hits),
        )

    def test_ci_yml_forbids_nonzero_retries(self) -> None:
        self.assertTrue(CI_YML.is_file(), f"missing {CI_YML}")
        hits = find_nonzero_retries(CI_YML)
        self.assertEqual(
            hits,
            [],
            "ci.yml 禁止 --retries N（N>0）。命中:\n" + "\n".join(hits),
        )


if __name__ == "__main__":
    unittest.main()
