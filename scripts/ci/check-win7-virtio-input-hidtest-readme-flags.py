#!/usr/bin/env python3
"""
CI guardrail: ensure Win7 virtio-input `hidtest` docs don't drift ahead of the
implemented flag surface area.

Why:
  - `hidtest` is a small, string-compare CLI tool; new flags are frequently added
    during bring-up/debugging.
  - It's easy for docs/playbooks to reference future/aspirational flags, or to
    forget to update docs when flags are renamed/removed.

The tool parses its CLI via string compares in:
  drivers/windows7/virtio-input/tools/hidtest/main.c

Primary docs:
  - drivers/windows7/virtio-input/tools/hidtest/README.md
  - drivers/windows7/virtio-input/tests/qemu/README.md

This script:
  1) extracts all accepted `--...` flags from the *argument parsing logic*
  2) checks that hidtest docs only reference flags that are actually parsed

Heuristics:
  - For hidtest's own README, we scan the entire file for `--...` tokens (it is
    a dedicated doc for the tool).
  - For other markdown files, we only consider lines that mention `hidtest.exe`
    to avoid false positives from unrelated tools (QEMU flags, test runners,
    etc).
"""

from __future__ import annotations

import pathlib
import re
import sys


def repo_root() -> pathlib.Path:
    # scripts/ci/<this file> -> scripts/ci -> scripts -> repo root
    return pathlib.Path(__file__).resolve().parents[2]


ROOT = repo_root()

HIDTEST_SRC = ROOT / "drivers" / "windows7" / "virtio-input" / "tools" / "hidtest" / "main.c"
HIDTEST_README = ROOT / "drivers" / "windows7" / "virtio-input" / "tools" / "hidtest" / "README.md"

# Markdown files to scan for *hidtest invocations* (not for any random `--...` tokens).
SCAN_MD_DIRS = [
    ROOT / "docs",
    ROOT / "instructions",
    ROOT / "drivers" / "windows7" / "virtio-input",
]

# Extract flags from the actual argument parsing logic (wcscmp on argv[i]), not
# from usage text.
CPP_FLAG_RE = re.compile(r'wcscmp\(\s*argv\[i\]\s*,\s*L"(--[A-Za-z0-9][A-Za-z0-9-]*)"\s*\)\s*==\s*0')

MD_FLAG_RE = re.compile(r"--[A-Za-z0-9][A-Za-z0-9-]*")


def read_text(path: pathlib.Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")


def iter_md_files(base: pathlib.Path) -> list[pathlib.Path]:
    if not base.exists():
        return []
    if base.is_file():
        return [base]
    return [p for p in base.rglob("*.md") if p.is_file()]


def iter_flag_refs_full_file(path: pathlib.Path) -> list[tuple[int, str]]:
    text = read_text(path)
    out: list[tuple[int, str]] = []
    for line_no, line in enumerate(text.splitlines(), start=1):
        for flag in MD_FLAG_RE.findall(line):
            out.append((line_no, flag))
    return out


def iter_flag_refs_hidtest_lines_only(path: pathlib.Path) -> list[tuple[int, str]]:
    text = read_text(path)
    out: list[tuple[int, str]] = []
    for line_no, line in enumerate(text.splitlines(), start=1):
        if "hidtest.exe" not in line:
            continue
        for flag in MD_FLAG_RE.findall(line):
            out.append((line_no, flag))
    return out


def main() -> int:
    errors: list[str] = []

    if not HIDTEST_SRC.exists():
        errors.append(f"Missing source file: {HIDTEST_SRC.relative_to(ROOT)}")
    if not HIDTEST_README.exists():
        errors.append(f"Missing README file: {HIDTEST_README.relative_to(ROOT)}")

    if errors:
        print("ERROR: hidtest README flag check failed due to missing inputs:", file=sys.stderr)
        for e in errors:
            print(f"  - {e}", file=sys.stderr)
        return 1

    src = read_text(HIDTEST_SRC)
    allowed_flags = set(CPP_FLAG_RE.findall(src))
    if not allowed_flags:
        rel = HIDTEST_SRC.relative_to(ROOT)
        print(f"ERROR: no flags extracted from {rel}; regex may be broken.", file=sys.stderr)
        return 1

    # 1) Validate hidtest README itself (full-file scan).
    readme_refs = iter_flag_refs_full_file(HIDTEST_README)
    unknown_readme = sorted({f for _, f in readme_refs} - allowed_flags)
    if unknown_readme:
        rel = HIDTEST_README.relative_to(ROOT)
        print(f"ERROR: {rel} references unknown flags:", file=sys.stderr)
        for unknown_flag in unknown_readme:
            for line_no, f in readme_refs:
                if f == unknown_flag:
                    print(f"  - {rel}:{line_no}: {unknown_flag}", file=sys.stderr)
        print("\nAllowed flags (extracted from hidtest/main.c):", file=sys.stderr)
        for f in sorted(allowed_flags):
            print(f"  - {f}", file=sys.stderr)
        return 1

    # 2) Validate other docs that mention hidtest invocations.
    doc_errors: list[str] = []
    for base in SCAN_MD_DIRS:
        for path in iter_md_files(base):
            if path == HIDTEST_README:
                continue
            refs = iter_flag_refs_hidtest_lines_only(path)
            if not refs:
                continue
            rel = path.relative_to(ROOT)
            for line_no, flag in refs:
                if flag not in allowed_flags:
                    doc_errors.append(f"{rel}:{line_no}: {flag}")

    if doc_errors:
        print("ERROR: hidtest docs reference unknown flags:", file=sys.stderr)
        for e in doc_errors:
            print(f"  - {e}", file=sys.stderr)
        print("\nAllowed flags (extracted from hidtest/main.c):", file=sys.stderr)
        for f in sorted(allowed_flags):
            print(f"  - {f}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())

