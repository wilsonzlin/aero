#!/usr/bin/env python3
"""
Sync the AeroGPU Win7 test runner fallback list with tests_manifest.txt.

The suite runner (`drivers/aerogpu/tests/win7/test_runner/main.cpp`) contains a
built-in `kFallbackTests[]` list used when `tests_manifest.txt` is not present
(e.g. binary-only distributions).

This script keeps that fallback list in lockstep with the canonical manifest:

  drivers/aerogpu/tests/win7/tests_manifest.txt

Usage:
  python3 scripts/sync-aerogpu-win7-test-runner-fallback-tests.py
  python3 scripts/sync-aerogpu-win7-test-runner-fallback-tests.py --check
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


def read_manifest_tests(path: Path) -> list[str]:
    tests: list[str] = []
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or line.startswith(";"):
            continue
        tests.append(line)
    return tests


def sync_runner_fallback_list(runner_text: str, tests: list[str]) -> str:
    m = re.search(
        r"(const\s+char\*\s+const\s+kFallbackTests\[\]\s*=\s*\{)(.*?)(^\s*\};)",
        runner_text,
        re.S | re.M,
    )
    if not m:
        raise RuntimeError("could not find kFallbackTests[] definition in runner source")

    body = m.group(2)
    indent_match = re.search(r"\n([ \t]*)\"[^\"]+\",\s*$", body, re.M)
    indent = indent_match.group(1) if indent_match else "      "

    new_body_lines = [f'{indent}"{name}",' for name in tests]
    new_body = "\n" + "\n".join(new_body_lines) + ("\n" if new_body_lines else "\n")

    return runner_text[: m.start(2)] + new_body + runner_text[m.end(2) :]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="Do not modify files; exit non-zero if an update would be made.",
    )
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    manifest_path = repo_root / "drivers/aerogpu/tests/win7/tests_manifest.txt"
    runner_path = repo_root / "drivers/aerogpu/tests/win7/test_runner/main.cpp"

    if not manifest_path.is_file():
        print(f"error: manifest not found: {manifest_path}", file=sys.stderr)
        return 2
    if not runner_path.is_file():
        print(f"error: runner source not found: {runner_path}", file=sys.stderr)
        return 2

    tests = read_manifest_tests(manifest_path)
    if not tests:
        print(f"error: manifest is empty: {manifest_path}", file=sys.stderr)
        return 2

    original = runner_path.read_text(encoding="utf-8", errors="replace")
    updated = sync_runner_fallback_list(original, tests)

    if updated == original:
        print("ok: kFallbackTests[] is already in sync with tests_manifest.txt")
        return 0

    if args.check:
        print(
            "error: kFallbackTests[] is out of sync with tests_manifest.txt; "
            "run this script without --check to update it",
            file=sys.stderr,
        )
        return 1

    runner_path.write_text(updated, encoding="utf-8")
    print(f"updated: {runner_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

