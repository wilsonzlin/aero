#!/usr/bin/env python3
"""
CI guardrail: ensure the Win7 `aerogpu_dbgctl` README doesn't drift ahead of the
implemented flag surface area.

We keep docs for this tool in-tree at:
  drivers/aerogpu/tools/win7_dbgctl/README.md

The tool itself is intentionally simple and parses its CLI via string compares
in:
  drivers/aerogpu/tools/win7_dbgctl/src/aerogpu_dbgctl.cpp

This script extracts all accepted `--...` flags from the tool source and fails
CI if the README references any `--...` flag that isn't actually parsed today.

Additionally, it checks the Win7 AeroGPU validation playbook's dbgctl command
section so bring-up docs can't accidentally reference non-existent flags.

Rationale: several docs/playbooks historically referenced future/aspirational
debug knobs (perf capture, hang injection, etc.). This guardrail ensures the
README remains an accurate "what exists" reference.
"""

from __future__ import annotations

import pathlib
import re
import sys


def repo_root() -> pathlib.Path:
    # scripts/ci/<this file> -> scripts/ci -> scripts -> repo root
    return pathlib.Path(__file__).resolve().parents[2]


ROOT = repo_root()

DBGCTL_SRC = ROOT / "drivers" / "aerogpu" / "tools" / "win7_dbgctl" / "src" / "aerogpu_dbgctl.cpp"
DBGCTL_README = ROOT / "drivers" / "aerogpu" / "tools" / "win7_dbgctl" / "README.md"
WIN7_VALIDATION_DOC = ROOT / "docs" / "graphics" / "win7-aerogpu-validation.md"

CPP_FLAG_RE = re.compile(r'L"(--[A-Za-z0-9][A-Za-z0-9-]*)"')
MD_FLAG_RE = re.compile(r"--[A-Za-z0-9][A-Za-z0-9-]*")


def extract_validation_dbgctl_section(text: str) -> tuple[str, int] | None:
    """
    Returns (section_text, start_line_number) for the validation doc's dbgctl
    section (5.2), or None if the section cannot be found.
    """
    m = re.search(
        r"^### 5\.2\b.*?\n(.*?)(?=^### 5\.3\b)",
        text,
        flags=re.MULTILINE | re.DOTALL,
    )
    if not m:
        return None
    start_line = text[: m.start(1)].count("\n") + 1
    return (m.group(1), start_line)


def read_text(path: pathlib.Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")


def main() -> int:
    errors: list[str] = []

    if not DBGCTL_SRC.exists():
        errors.append(f"Missing source file: {DBGCTL_SRC.relative_to(ROOT)}")
    if not DBGCTL_README.exists():
        errors.append(f"Missing README file: {DBGCTL_README.relative_to(ROOT)}")
    if not WIN7_VALIDATION_DOC.exists():
        errors.append(f"Missing Win7 validation doc: {WIN7_VALIDATION_DOC.relative_to(ROOT)}")

    if errors:
        print("ERROR: Win7 dbgctl README flag check failed due to missing inputs:", file=sys.stderr)
        for e in errors:
            print(f"  - {e}", file=sys.stderr)
        return 1

    src = read_text(DBGCTL_SRC)
    allowed_flags = set(CPP_FLAG_RE.findall(src))

    if not allowed_flags:
        print(
            f"ERROR: No flags extracted from {DBGCTL_SRC.relative_to(ROOT)}; regex may be broken.",
            file=sys.stderr,
        )
        return 1

    readme = read_text(DBGCTL_README)
    referenced_flags = set(MD_FLAG_RE.findall(readme))

    unknown = sorted(referenced_flags - allowed_flags)
    if unknown:
        print(
            "ERROR: drivers/aerogpu/tools/win7_dbgctl/README.md references unknown flags:",
            file=sys.stderr,
        )
        for f in unknown:
            print(f"  - {f}", file=sys.stderr)
        print("\nAllowed flags (extracted from aerogpu_dbgctl.cpp):", file=sys.stderr)
        for f in sorted(allowed_flags):
            print(f"  - {f}", file=sys.stderr)
        return 1

    # Also ensure the Win7 validation playbook's dbgctl section only references
    # real flags (so it can't drift ahead of implementation).
    validation_text = read_text(WIN7_VALIDATION_DOC)
    extracted = extract_validation_dbgctl_section(validation_text)
    if not extracted:
        print(
            f"ERROR: failed to locate the dbgctl command section (### 5.2 .. ### 5.3) in {WIN7_VALIDATION_DOC.relative_to(ROOT)}",
            file=sys.stderr,
        )
        return 1

    section_text, section_start_line = extracted
    validation_flags = set(MD_FLAG_RE.findall(section_text))
    unknown_validation = sorted(validation_flags - allowed_flags)
    if unknown_validation:
        rel = WIN7_VALIDATION_DOC.relative_to(ROOT)
        print(f"ERROR: {rel} dbgctl section references unknown flags:", file=sys.stderr)
        for f in unknown_validation:
            # Emit best-effort line numbers relative to the full file.
            for idx, line in enumerate(section_text.splitlines(), start=0):
                if f in line:
                    print(f"  - {rel}:{section_start_line + idx}: {f}", file=sys.stderr)
                    break
            else:
                print(f"  - {rel}:?: {f}", file=sys.stderr)

        print("\nAllowed flags (extracted from aerogpu_dbgctl.cpp):", file=sys.stderr)
        for f in sorted(allowed_flags):
            print(f"  - {f}", file=sys.stderr)
        return 1

    print("OK: Win7 dbgctl README flags match parsed implementation.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
