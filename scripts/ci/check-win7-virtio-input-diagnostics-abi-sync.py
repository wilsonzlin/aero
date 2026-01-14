#!/usr/bin/env python3
"""
CI guardrail: ensure Win7 virtio-input user-mode tooling stays in sync with the
driver-visible diagnostics ABI.

Why:
  - The virtio-input driver exposes a small set of private IOCTLs
    (IOCTL_VIOINPUT_QUERY_COUNTERS / IOCTL_VIOINPUT_QUERY_STATE) whose output
    structs are user-mode visible:
      - VIOINPUT_COUNTERS
      - VIOINPUT_STATE
  - `drivers/windows7/virtio-input/tools/hidtest/main.c` intentionally duplicates
    these struct layouts so it can be built with non-WDK toolchains (SDK or
    MinGW-w64) without including the driver's WDK-only headers.

This script prevents accidental drift between:
  - the driver source of truth: `drivers/windows7/virtio-input/src/log.h`
  - the user-mode tooling copies: `drivers/windows7/virtio-input/tools/hidtest/main.c`

It checks:
  - VIOINPUT_COUNTERS_VERSION / VIOINPUT_STATE_VERSION match
  - field order/name lists for VIOINPUT_COUNTERS and VIOINPUT_STATE match

Note: it intentionally ignores qualifiers like `volatile`, and does not attempt
to validate packing/alignment beyond field order/name equivalence (the structs
are composed of simple scalar fields).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
LOG_H = REPO_ROOT / "drivers/windows7/virtio-input/src/log.h"
HIDTEST_C = REPO_ROOT / "drivers/windows7/virtio-input/tools/hidtest/main.c"


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def strip_c_comments(text: str) -> str:
    # Remove /* ... */ first so we don't accidentally kill // inside a block comment.
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    # Remove // ... comments.
    text = re.sub(r"//.*?$", "", text, flags=re.M)
    return text


def extract_define_int(text: str, macro: str, *, file: Path) -> int:
    m = re.search(rf"^\s*#define\s+{re.escape(macro)}\s+(\d+)\s*[uUlL]*\b", text, flags=re.M)
    if not m:
        fail(f"{file.as_posix()}: missing '#define {macro} <int>'")
    return int(m.group(1))


def extract_struct_body(text: str, typedef_regex: str, *, file: Path, what: str) -> str:
    """
    Extract the substring inside the outermost braces for a `typedef struct ... { ... }` match.

    `typedef_regex` must match up to and including the opening `{`.
    """
    m = re.search(typedef_regex, text, flags=re.S)
    if not m:
        fail(f"{file.as_posix()}: could not find {what}")

    # Start scanning right after the first '{' in the match.
    i = m.end() - 1
    if i < 0 or text[i] != "{":
        fail(f"{file.as_posix()}: internal parse error: expected '{{' after {what}")

    depth = 0
    start = i + 1
    for j in range(i, len(text)):
        ch = text[j]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return text[start:j]

    fail(f"{file.as_posix()}: unterminated struct body for {what}")
    raise AssertionError("unreachable")


FIELD_RE = re.compile(
    r"(?m)^\s*(?:volatile\s+)?(?:ULONG|LONG|UINT64|ULONGLONG)\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
)


def extract_field_names(struct_body: str) -> list[str]:
    return FIELD_RE.findall(struct_body)


def compare_field_lists(*, driver_fields: list[str], tool_fields: list[str], label: str) -> list[str]:
    failures: list[str] = []

    if driver_fields == tool_fields:
        return failures

    failures.append(f"{label}: field list mismatch")
    failures.append(f"  driver field count : {len(driver_fields)}")
    failures.append(f"  hidtest field count: {len(tool_fields)}")

    # Find first mismatch index for readability.
    n = min(len(driver_fields), len(tool_fields))
    for i in range(n):
        if driver_fields[i] != tool_fields[i]:
            failures.append(f"  first mismatch at index {i}: driver={driver_fields[i]!r} hidtest={tool_fields[i]!r}")
            break
    else:
        if len(driver_fields) != len(tool_fields):
            failures.append("  field lists differ only by trailing fields")

    # Include full lists (compact) to make CI failures actionable.
    failures.append(f"  driver fields : {', '.join(driver_fields)}")
    failures.append(f"  hidtest fields: {', '.join(tool_fields)}")
    return failures


def main() -> int:
    if not LOG_H.exists():
        fail(f"missing expected file: {LOG_H.as_posix()}")
    if not HIDTEST_C.exists():
        fail(f"missing expected file: {HIDTEST_C.as_posix()}")

    driver_text = strip_c_comments(LOG_H.read_text(encoding="utf-8", errors="replace"))
    tool_text = strip_c_comments(HIDTEST_C.read_text(encoding="utf-8", errors="replace"))

    failures: list[str] = []

    driver_counters_version = extract_define_int(driver_text, "VIOINPUT_COUNTERS_VERSION", file=LOG_H)
    tool_counters_version = extract_define_int(tool_text, "VIOINPUT_COUNTERS_VERSION", file=HIDTEST_C)
    if driver_counters_version != tool_counters_version:
        failures.append(
            f"VIOINPUT_COUNTERS_VERSION mismatch: driver={driver_counters_version} hidtest={tool_counters_version}"
        )

    driver_state_version = extract_define_int(driver_text, "VIOINPUT_STATE_VERSION", file=LOG_H)
    tool_state_version = extract_define_int(tool_text, "VIOINPUT_STATE_VERSION", file=HIDTEST_C)
    if driver_state_version != tool_state_version:
        failures.append(f"VIOINPUT_STATE_VERSION mismatch: driver={driver_state_version} hidtest={tool_state_version}")

    driver_counters_body = extract_struct_body(
        driver_text,
        r"\btypedef\s+struct\s+_VIOINPUT_COUNTERS\s*\{",
        file=LOG_H,
        what="typedef struct _VIOINPUT_COUNTERS { ... }",
    )
    tool_counters_body = extract_struct_body(
        tool_text,
        r"\btypedef\s+struct\s+_VIOINPUT_COUNTERS\s*\{",
        file=HIDTEST_C,
        what="typedef struct _VIOINPUT_COUNTERS { ... } (hidtest)",
    )

    driver_counters_fields = extract_field_names(driver_counters_body)
    tool_counters_fields = extract_field_names(tool_counters_body)
    failures += compare_field_lists(
        driver_fields=driver_counters_fields,
        tool_fields=tool_counters_fields,
        label="VIOINPUT_COUNTERS",
    )

    driver_state_body = extract_struct_body(
        driver_text,
        r"\btypedef\s+struct\s+_VIOINPUT_STATE\s*\{",
        file=LOG_H,
        what="typedef struct _VIOINPUT_STATE { ... }",
    )
    tool_state_body = extract_struct_body(
        tool_text,
        r"\btypedef\s+struct\s+VIOINPUT_STATE\s*\{",
        file=HIDTEST_C,
        what="typedef struct VIOINPUT_STATE { ... } (hidtest)",
    )

    driver_state_fields = extract_field_names(driver_state_body)
    tool_state_fields = extract_field_names(tool_state_body)
    failures += compare_field_lists(
        driver_fields=driver_state_fields,
        tool_fields=tool_state_fields,
        label="VIOINPUT_STATE",
    )

    if failures:
        print("error: Win7 virtio-input diagnostics ABI is out of sync:\n", file=sys.stderr)
        for line in failures:
            print(line, file=sys.stderr)
        print(
            "\nFix: keep these in sync:\n"
            "  - drivers/windows7/virtio-input/src/log.h\n"
            "  - drivers/windows7/virtio-input/tools/hidtest/main.c\n",
            file=sys.stderr,
        )
        return 1

    print(
        "ok: Win7 virtio-input diagnostics ABI is in sync "
        f"(counters v{driver_counters_version}, state v{driver_state_version})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

