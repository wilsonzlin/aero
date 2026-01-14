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

Additionally, it checks dbgctl invocations in the Win7 AeroGPU validation
playbook (and other docs that mention `aerogpu_dbgctl`) so bring-up docs can't
accidentally reference non-existent flags.

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

# Extract flags from the actual argument parsing logic, not from the usage text,
# so we catch cases where someone updates the help/README but forgets to plumb
# the flag through the parser.
#
# Most flags are parsed via `wcscmp(a, L"--foo")`, but some options accept
# `--flag=value` forms via prefix matching (e.g. `wcsncmp(a, L"--json=", 7)`).
CPP_FLAG_WCSCMP_RE = re.compile(r'wcscmp\(\s*a\s*,\s*L"(--[A-Za-z0-9][A-Za-z0-9-]*)"\s*\)')
CPP_FLAG_WCSNCMP_EQ_RE = re.compile(r'wcsncmp\(\s*a\s*,\s*L"(--[A-Za-z0-9][A-Za-z0-9-]*)="\s*,')
MD_FLAG_RE = re.compile(r"--[A-Za-z0-9][A-Za-z0-9-]*")

# We only scan markdown files in these dirs for dbgctl invocations.
SCAN_MD_DIRS = [
    ROOT / "docs",
    # Project-wide playbooks live under instructions/ and may embed dbgctl invocations.
    ROOT / "instructions",
    ROOT / "drivers" / "aerogpu",
    # Guest Tools docs embed dbgctl invocations (e.g. `--status --timeout-ms 2000`) and
    # should also be kept in sync with the actual dbgctl flag surface.
    ROOT / "guest-tools",
    # CI docs sometimes include dbgctl usage snippets / paths.
    ROOT / "ci",
]


def extract_md_section(text: str, patterns: list[str]) -> tuple[str, int] | None:
    """
    Returns (section_text, start_line_number) for the first matching section
    regex in `patterns`.

    Each pattern must capture the desired section content in group(1).
    """
    for pattern in patterns:
        m = re.search(pattern, text, flags=re.MULTILINE | re.DOTALL)
        if not m:
            continue
        start_line = text[: m.start(1)].count("\n") + 1
        return (m.group(1), start_line)
    return None


def extract_validation_dbgctl_section(text: str) -> tuple[str, int] | None:
    """
    Returns (section_text, start_line_number) for the validation doc's dbgctl
    section (currently 5.2), or None if the section cannot be found.

    We primarily key off section numbers (stable today), but also include a
    title-based fallback so minor renumbering doesn't break CI.
    """
    return extract_md_section(
        text,
        patterns=[
            r"^### 5\.2\b.*?\n(.*?)(?=^### 5\.3\b)",
            r"^### .*\bTypical workflow\b.*?\n(.*?)(?=^### .*\bSuggested\s+`?aerogpu_dbgctl`?\s+commands\b)",
        ],
    )


def extract_validation_dbgctl_table_section(text: str) -> tuple[str, int] | None:
    """
    Returns (section_text, start_line_number) for the validation doc's dbgctl
    command table section (5.3), or None if the section cannot be found.

    This section contains a markdown table of commands where the `--...` flags
    are listed without the `aerogpu_dbgctl` prefix, so it is not covered by the
    line-based `aerogpu_dbgctl` invocation heuristic.
    """
    return extract_md_section(
        text,
        patterns=[
            r"^### 5\.3\b.*?\n(.*?)(?=^### 5\.4\b)",
            r"^### .*\bSuggested\s+`?aerogpu_dbgctl`?\s+commands\b.*?\n(.*?)(?=^### .*\bCommon error codes\b)",
        ],
    )


def extract_validation_dbgctl_error_section(text: str) -> tuple[str, int] | None:
    """
    Returns (section_text, start_line_number) for the validation doc's error
    code table section (currently 5.4), or None if the section cannot be found.

    This section occasionally references dbgctl flags without the `aerogpu_dbgctl`
    prefix (for example in the "First checks" column), so validate it explicitly.
    """
    return extract_md_section(
        text,
        patterns=[
            r"^### 5\.4\b.*?\n(.*?)(?=^### 5\.5\b)",
            r"^### .*\bCommon error codes and likely causes\b.*?\n(.*?)(?=^### .*\bBackend/submission failure\b)",
        ],
    )


def iter_dbgctl_flag_refs(path: pathlib.Path) -> list[tuple[int, str]]:
    """
    Find referenced dbgctl `--...` flags in places that appear to be dbgctl
    invocations.

    We intentionally do *not* scan the full file for `--...` because many docs
    include flags for other tools (test runners, scripts, etc.).

    Current heuristic:
    - only consider lines that mention `aerogpu_dbgctl` (or `aerogpu_dbgctl.exe`)
    - skip lines that mention `--dbgctl` (test-runner flags that reference the
      dbgctl binary by path, but are not dbgctl CLI flags)
    """
    text = read_text(path)
    out: list[tuple[int, str]] = []
    for line_no, line in enumerate(text.splitlines(), start=1):
        if "aerogpu_dbgctl" not in line:
            continue
        if "--dbgctl" in line:
            continue
        for flag in MD_FLAG_RE.findall(line):
            out.append((line_no, flag))
    return out


def iter_md_files(base: pathlib.Path) -> list[pathlib.Path]:
    if not base.exists():
        return []
    if base.is_file():
        return [base]
    return [p for p in base.rglob("*.md") if p.is_file()]


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
    allowed_flags = set(CPP_FLAG_WCSCMP_RE.findall(src)) | set(CPP_FLAG_WCSNCMP_EQ_RE.findall(src))

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

    # Also ensure the Win7 validation playbook doesn't reference unknown dbgctl
    # flags in its dbgctl sections.
    validation_text = read_text(WIN7_VALIDATION_DOC)

    extracted = extract_validation_dbgctl_section(validation_text)
    extracted_table = extract_validation_dbgctl_table_section(validation_text)
    extracted_error = extract_validation_dbgctl_error_section(validation_text)
    rel = WIN7_VALIDATION_DOC.relative_to(ROOT)

    if not extracted:
        print(f"ERROR: failed to locate dbgctl section (### 5.2 .. ### 5.3) in {rel}", file=sys.stderr)
        return 1
    if not extracted_table:
        print(f"ERROR: failed to locate dbgctl table section (### 5.3 .. ### 5.4) in {rel}", file=sys.stderr)
        return 1
    if not extracted_error:
        print(f"ERROR: failed to locate dbgctl error table section (### 5.4 .. ### 5.5) in {rel}", file=sys.stderr)
        return 1

    section_text, section_start_line = extracted
    section_refs: list[tuple[int, str]] = []
    for idx, line in enumerate(section_text.splitlines(), start=0):
        for flag in MD_FLAG_RE.findall(line):
            section_refs.append((section_start_line + idx, flag))

    unknown_section = sorted({f for _, f in section_refs} - allowed_flags)
    if unknown_section:
        print(f"ERROR: {rel} dbgctl section references unknown flags:", file=sys.stderr)
        for unknown_flag in unknown_section:
            for line_no, f in section_refs:
                if f == unknown_flag:
                    print(f"  - {rel}:{line_no}: {unknown_flag}", file=sys.stderr)
        print("\nAllowed flags (extracted from aerogpu_dbgctl.cpp):", file=sys.stderr)
        for f in sorted(allowed_flags):
            print(f"  - {f}", file=sys.stderr)
        return 1

    table_text, table_start_line = extracted_table
    table_refs: list[tuple[int, str]] = []
    for idx, line in enumerate(table_text.splitlines(), start=0):
        for flag in MD_FLAG_RE.findall(line):
            table_refs.append((table_start_line + idx, flag))

    unknown_table = sorted({f for _, f in table_refs} - allowed_flags)
    if unknown_table:
        print(f"ERROR: {rel} dbgctl command table references unknown flags:", file=sys.stderr)
        for unknown_flag in unknown_table:
            for line_no, f in table_refs:
                if f == unknown_flag:
                    print(f"  - {rel}:{line_no}: {unknown_flag}", file=sys.stderr)
        print("\nAllowed flags (extracted from aerogpu_dbgctl.cpp):", file=sys.stderr)
        for f in sorted(allowed_flags):
            print(f"  - {f}", file=sys.stderr)
        return 1

    error_text, error_start_line = extracted_error
    error_refs: list[tuple[int, str]] = []
    for idx, line in enumerate(error_text.splitlines(), start=0):
        for flag in MD_FLAG_RE.findall(line):
            error_refs.append((error_start_line + idx, flag))

    unknown_error = sorted({f for _, f in error_refs} - allowed_flags)
    if unknown_error:
        print(f"ERROR: {rel} dbgctl error table section references unknown flags:", file=sys.stderr)
        for unknown_flag in unknown_error:
            for line_no, f in error_refs:
                if f == unknown_flag:
                    print(f"  - {rel}:{line_no}: {unknown_flag}", file=sys.stderr)
        print("\nAllowed flags (extracted from aerogpu_dbgctl.cpp):", file=sys.stderr)
        for f in sorted(allowed_flags):
            print(f"  - {f}", file=sys.stderr)
        return 1

    # Check dbgctl invocations across docs (only in lines that look like dbgctl
    # usage, to avoid false positives from unrelated tools).
    doc_errors: list[str] = []
    for base in SCAN_MD_DIRS:
        for path in iter_md_files(base):
            refs = iter_dbgctl_flag_refs(path)
            if not refs:
                continue
            unknown_flags = sorted({f for _, f in refs} - allowed_flags)
            if not unknown_flags:
                continue
            rel = path.relative_to(ROOT)
            for unknown_flag in unknown_flags:
                for line_no, f in refs:
                    if f == unknown_flag:
                        doc_errors.append(f"{rel}:{line_no}: {unknown_flag}")

    if doc_errors:
        print("ERROR: docs reference unknown aerogpu_dbgctl flags:", file=sys.stderr)
        for e in sorted(doc_errors):
            print(f"  - {e}", file=sys.stderr)
        print("\nAllowed flags (extracted from aerogpu_dbgctl.cpp):", file=sys.stderr)
        for f in sorted(allowed_flags):
            print(f"  - {f}", file=sys.stderr)
        return 1

    print("OK: Win7 dbgctl docs flags match parsed implementation.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
