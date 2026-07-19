#!/usr/bin/env python3
"""coverage_assert.py 单元测试（无 cargo 依赖）。"""
from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

CI_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(CI_DIR))

from coverage_assert import aggregate, crate_of, main  # type: ignore  # noqa: E402


class TestCrateOf(unittest.TestCase):
    def test_crates_path(self) -> None:
        crates = ["tachyon-core", "tachyon-engine"]
        self.assertEqual(
            crate_of("/repo/crates/tachyon-core/src/lib.rs", crates),
            "tachyon-core",
        )

    def test_ignore_unrelated(self) -> None:
        crates = ["tachyon-core"]
        self.assertIsNone(crate_of("/repo/crates/other/src/lib.rs", crates))


class TestAggregate(unittest.TestCase):
    def test_per_crate_percent(self) -> None:
        files = [
            {
                "filename": "/x/crates/tachyon-core/src/a.rs",
                "summary": {"regions": {"count": 100, "covered": 95}},
            },
            {
                "filename": "/x/crates/tachyon-engine/src/b.rs",
                "summary": {"regions": {"count": 50, "covered": 40}},
            },
            {
                "filename": "/x/crates/tachyon-core/src/test_harness.rs",
                "summary": {"regions": {"count": 1000, "covered": 0}},
            },
        ]
        import re

        stats = aggregate(
            files,
            ["tachyon-core", "tachyon-engine"],
            re.compile("test_harness"),
        )
        self.assertAlmostEqual(stats["tachyon-core"]["percent"], 95.0)
        self.assertAlmostEqual(stats["tachyon-engine"]["percent"], 80.0)

    def test_main_fail_under(self) -> None:
        payload = {
            "data": [
                {
                    "files": [
                        {
                            "filename": "/x/crates/tachyon-core/src/a.rs",
                            "summary": {"regions": {"count": 100, "covered": 80}},
                        }
                    ]
                }
            ]
        }
        with tempfile.TemporaryDirectory() as td:
            p = Path(td) / "c.json"
            p.write_text(json.dumps(payload), encoding="utf-8")
            rc = main(
                [
                    str(p),
                    "--threshold",
                    "90",
                    "--crates",
                    "tachyon-core",
                ]
            )
            self.assertEqual(rc, 1)

    def test_main_pass(self) -> None:
        payload = {
            "data": [
                {
                    "files": [
                        {
                            "filename": "/x/crates/tachyon-core/src/a.rs",
                            "summary": {"regions": {"count": 100, "covered": 91}},
                        }
                    ]
                }
            ]
        }
        with tempfile.TemporaryDirectory() as td:
            p = Path(td) / "c.json"
            p.write_text(json.dumps(payload), encoding="utf-8")
            rc = main(
                [
                    str(p),
                    "--threshold",
                    "90",
                    "--crates",
                    "tachyon-core",
                ]
            )
            self.assertEqual(rc, 0)


if __name__ == "__main__":
    unittest.main()
