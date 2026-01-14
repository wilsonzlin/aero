#!/usr/bin/env python3
"""
CI guardrail: ensure Win7 virtio-input user-mode tooling stays in sync with the
driver-visible diagnostics ABI.

Why:
  - The virtio-input driver exposes a small set of private IOCTLs
    (IOCTL_VIOINPUT_QUERY_COUNTERS / IOCTL_VIOINPUT_QUERY_STATE /
    IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO) whose output
    structs are user-mode visible:
      - VIOINPUT_COUNTERS
      - VIOINPUT_STATE
      - VIOINPUT_INTERRUPT_INFO
  - Several user-mode programs intentionally duplicate these struct layouts so they
    can be built without including the driver's WDK-only headers:
      - `drivers/windows7/virtio-input/tools/hidtest/main.c`
      - `drivers/windows7/tests/guest-selftest/src/main.cpp`

This script prevents accidental drift between:
  - the driver source of truth:
      - `drivers/windows7/virtio-input/src/log.h` (diagnostics IOCTL ABI)
      - `drivers/windows7/virtio-input/src/virtio_input.h` (VIOINPUT_DEVICE_KIND ABI)
  - the user-mode copies listed above.

It checks:
  - VIOINPUT_COUNTERS_VERSION / VIOINPUT_STATE_VERSION / VIOINPUT_INTERRUPT_INFO_VERSION match
  - IOCTL_VIOINPUT_* CTL_CODE() definitions match (so tools/selftest don't drift to the wrong function codes)
  - field order/name lists for VIOINPUT_COUNTERS / VIOINPUT_STATE / VIOINPUT_INTERRUPT_INFO match
  - interrupt-info enum values / sentinel constants match (prevents user-mode tools from misinterpreting fields)
  - VIOINPUT_DEVICE_KIND values stay aligned with the driver enum in virtio_input.h

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
VIRTIO_INPUT_H = REPO_ROOT / "drivers/windows7/virtio-input/src/virtio_input.h"
GUEST_SELFTEST_CPP = REPO_ROOT / "drivers/windows7/tests/guest-selftest/src/main.cpp"


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


def extract_define_int_literal(text: str, macro: str, *, file: Path) -> int:
    """
    Extract a numeric value from a macro definition.

    This is intentionally more permissive than `extract_define_int`: it supports
    integer literals wrapped in simple casts/parentheses (e.g. `((USHORT)0xFFFF)`).
    """

    m = re.search(rf"(?m)^\s*#define\s+{re.escape(macro)}\b\s+(.+)$", text)
    if not m:
        fail(f"{file.as_posix()}: missing '#define {macro} <expr>'")

    expr = m.group(1).strip()

    # Accept exactly one integer literal in the expression.
    lits = re.findall(r"0x[0-9A-Fa-f]+|\d+", expr)
    if len(lits) != 1:
        fail(f"{file.as_posix()}: could not extract a single integer literal from {macro}: {expr!r}")
    return int(lits[0], 0)


def extract_const_int_literal(text: str, name: str, *, file: Path) -> int:
    """
    Extract an integer literal from a C++ constant definition like:
        static constexpr USHORT VIOINPUT_INTERRUPT_VECTOR_NONE = 0xFFFFu;

    This keeps the script compatible with user-mode helpers that intentionally avoid
    WDK-only headers (and therefore may not use `#define` macros).
    """

    m = re.search(
        rf"(?m)^\s*(?:static\s+)?(?:constexpr\s+)?[A-Za-z_][A-Za-z0-9_]*\s+{re.escape(name)}\s*=\s*(.+?);",
        text,
    )
    if not m:
        fail(f"{file.as_posix()}: missing {name} constant definition")

    expr = m.group(1).strip()
    lits = re.findall(r"0x[0-9A-Fa-f]+|\d+", expr)
    if len(lits) != 1:
        fail(f"{file.as_posix()}: could not extract a single integer literal from {name}: {expr!r}")
    return int(lits[0], 0)


def extract_constexpr_ctl_code_args(text: str, name: str, *, file: Path) -> tuple[str, str, str, str]:
    """
    Extract the 4 CTL_CODE() arguments from a C++ constant definition like:
        static constexpr DWORD IOCTL_FOO = CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_READ_ACCESS);
    or (split across lines):
        static constexpr DWORD IOCTL_FOO =
            CTL_CODE(...);

    Returns a tuple of the 4 arguments as normalized strings (whitespace stripped).
    """

    m = re.search(
        rf"(?m)^\s*(?:static\s+)?(?:constexpr\s+)?(?:DWORD|ULONG|UINT32|uint32_t|unsigned\s+long)\s+"
        rf"{re.escape(name)}\s*=\s*(?:\r?\n\s*)?CTL_CODE\s*\(\s*([^\)]*?)\s*\)\s*;",
        text,
        flags=re.S,
    )
    if not m:
        fail(f"{file.as_posix()}: missing {name} CTL_CODE(...) constexpr definition")

    args_str = m.group(1)
    parts = [p.strip() for p in args_str.split(",")]
    if len(parts) != 4:
        fail(f"{file.as_posix()}: could not parse 4 CTL_CODE args for {name} (got {len(parts)}): {parts!r}")
    return (parts[0], parts[1], parts[2], parts[3])


def extract_ctl_code_args(text: str, macro: str, *, file: Path) -> tuple[str, str, str, str]:
    """
    Extract the 4 CTL_CODE() arguments from a macro definition like:
        #define IOCTL_FOO \\n
            CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_READ_ACCESS)

    Returns a tuple of the 4 arguments as normalized strings (whitespace stripped).
    """

    m = re.search(
        rf"(?m)^\s*#define\s+{re.escape(macro)}\b[\s\\]*CTL_CODE\s*\(\s*([^\)]*?)\s*\)",
        text,
        flags=re.S,
    )
    if not m:
        fail(f"{file.as_posix()}: missing '#define {macro} CTL_CODE(...)'")

    args_str = m.group(1)
    # Remove explicit line continuation backslashes for robustness.
    args_str = args_str.replace("\\", " ")
    parts = [p.strip() for p in args_str.split(",")]
    if len(parts) != 4:
        fail(f"{file.as_posix()}: could not parse 4 CTL_CODE args for {macro} (got {len(parts)}): {parts!r}")
    return (parts[0], parts[1], parts[2], parts[3])


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


def parse_enum_items(body: str, *, file: Path, what: str) -> list[tuple[str, int]]:
    """
    Parse an enum body ("NAME = 0, NAME2, ...") into a list of (name, value).
    Supports both explicit and implicit value assignment.
    """

    items: list[tuple[str, int]] = []
    current = -1

    for raw in body.split(","):
        entry = raw.strip()
        if not entry:
            continue

        m = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)(?:\s*=\s*(.+))?$", entry)
        if not m:
            fail(f"{file.as_posix()}: could not parse {what} enum entry: {entry!r}")

        name = m.group(1)
        value_expr = m.group(2)
        if value_expr is None:
            current += 1
            value = current
        else:
            ve = value_expr.strip()
            m2 = re.fullmatch(r"([-+]?(?:0x[0-9A-Fa-f]+|\d+))(?:[uUlL]+)?", ve)
            if not m2:
                fail(f"{file.as_posix()}: unsupported {what} enum value expression: {ve!r}")
            value = int(m2.group(1), 0)
            current = value

        items.append((name, value))

    if not items:
        fail(f"{file.as_posix()}: {what} enum has no entries")
    return items


FIELD_RE = re.compile(
    r"(?m)^\s*(?:volatile\s+)?"
    r"(?:[A-Za-z_][A-Za-z0-9_]*)"
    r"(?:\s+|\s*\*+)"
    r"([A-Za-z_][A-Za-z0-9_]*)"
    r"\s*(?:\[[^\]]*\])?\s*;"
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


def format_enum_items(items: list[tuple[str, int]]) -> str:
    return ", ".join(f"{name}={value} (0x{value:X})" for name, value in items)


def compare_enum_item_lists(
    *, driver_items: list[tuple[str, int]], tool_items: list[tuple[str, int]], label: str
) -> list[str]:
    failures: list[str] = []

    if driver_items == tool_items:
        return failures

    failures.append(f"{label}: enum values mismatch")
    failures.append(f"  driver item count : {len(driver_items)}")
    failures.append(f"  hidtest item count: {len(tool_items)}")

    # Find first mismatch index for readability.
    n = min(len(driver_items), len(tool_items))
    for i in range(n):
        if driver_items[i] != tool_items[i]:
            d_name, d_val = driver_items[i]
            t_name, t_val = tool_items[i]
            failures.append(
                f"  first mismatch at index {i}: "
                f"driver={d_name}={d_val} (0x{d_val:X}) hidtest={t_name}={t_val} (0x{t_val:X})"
            )
            break
    else:
        if len(driver_items) != len(tool_items):
            failures.append("  enum lists differ only by trailing items")

    failures.append(f"  driver items : {format_enum_items(driver_items)}")
    failures.append(f"  hidtest items: {format_enum_items(tool_items)}")
    return failures


def main() -> int:
    if not LOG_H.exists():
        fail(f"missing expected file: {LOG_H.as_posix()}")
    if not HIDTEST_C.exists():
        fail(f"missing expected file: {HIDTEST_C.as_posix()}")
    if not VIRTIO_INPUT_H.exists():
        fail(f"missing expected file: {VIRTIO_INPUT_H.as_posix()}")
    if not GUEST_SELFTEST_CPP.exists():
        fail(f"missing expected file: {GUEST_SELFTEST_CPP.as_posix()}")

    driver_text = strip_c_comments(LOG_H.read_text(encoding="utf-8", errors="replace"))
    tool_text = strip_c_comments(HIDTEST_C.read_text(encoding="utf-8", errors="replace"))
    virtio_input_text = strip_c_comments(VIRTIO_INPUT_H.read_text(encoding="utf-8", errors="replace"))
    selftest_text = strip_c_comments(GUEST_SELFTEST_CPP.read_text(encoding="utf-8", errors="replace"))

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

    driver_interrupt_version = extract_define_int(driver_text, "VIOINPUT_INTERRUPT_INFO_VERSION", file=LOG_H)
    tool_interrupt_version = extract_define_int(tool_text, "VIOINPUT_INTERRUPT_INFO_VERSION", file=HIDTEST_C)
    if driver_interrupt_version != tool_interrupt_version:
        failures.append(
            "VIOINPUT_INTERRUPT_INFO_VERSION mismatch: "
            f"driver={driver_interrupt_version} hidtest={tool_interrupt_version}"
        )

    # IOCTL definitions are user-mode visible API. Ensure hidtest stays in sync with the
    # driver's canonical CTL_CODE choices.
    ioctl_macros = (
        "IOCTL_VIOINPUT_QUERY_COUNTERS",
        "IOCTL_VIOINPUT_RESET_COUNTERS",
        "IOCTL_VIOINPUT_QUERY_STATE",
        "IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO",
        # Diagnostics-only (still checked for header/tooling drift).
        "IOCTL_VIOINPUT_GET_LOG_MASK",
        "IOCTL_VIOINPUT_SET_LOG_MASK",
    )
    for name in ioctl_macros:
        driver_args = extract_ctl_code_args(driver_text, name, file=LOG_H)
        tool_args = extract_ctl_code_args(tool_text, name, file=HIDTEST_C)
        if driver_args != tool_args:
            failures.append(
                f"{name} CTL_CODE mismatch:\n"
                f"  driver: {driver_args}\n"
                f"  hidtest: {tool_args}"
            )

    # The Win7 guest selftest duplicates a subset of these IOCTLs as C++ constexpr values so it can be
    # built without including WDK-only headers. Keep those in sync too.
    selftest_ioctl_names = (
        "IOCTL_VIOINPUT_QUERY_COUNTERS",
        "IOCTL_VIOINPUT_RESET_COUNTERS",
        "IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO",
    )
    for name in selftest_ioctl_names:
        driver_args = extract_ctl_code_args(driver_text, name, file=LOG_H)
        selftest_args = extract_constexpr_ctl_code_args(selftest_text, name, file=GUEST_SELFTEST_CPP)
        if driver_args != selftest_args:
            failures.append(
                f"{name} CTL_CODE mismatch:\n"
                f"  driver: {driver_args}\n"
                f"  guest-selftest: {selftest_args}"
            )

    # Interrupt-info enum values / constants (user-mode visible ABI).
    driver_int_mode_body = extract_struct_body(
        driver_text,
        r"\btypedef\s+enum\s+_VIOINPUT_INTERRUPT_MODE\s*\{",
        file=LOG_H,
        what="typedef enum _VIOINPUT_INTERRUPT_MODE { ... }",
    )
    tool_int_mode_body = extract_struct_body(
        tool_text,
        r"\btypedef\s+enum\s+_VIOINPUT_INTERRUPT_MODE\s*\{",
        file=HIDTEST_C,
        what="typedef enum _VIOINPUT_INTERRUPT_MODE { ... } (hidtest)",
    )
    failures += compare_enum_item_lists(
        driver_items=parse_enum_items(driver_int_mode_body, file=LOG_H, what="VIOINPUT_INTERRUPT_MODE"),
        tool_items=parse_enum_items(tool_int_mode_body, file=HIDTEST_C, what="VIOINPUT_INTERRUPT_MODE"),
        label="VIOINPUT_INTERRUPT_MODE",
    )

    selftest_int_mode_body = extract_struct_body(
        selftest_text,
        r"\benum\s+VIOINPUT_INTERRUPT_MODE\b[^{]*\{",
        file=GUEST_SELFTEST_CPP,
        what="enum VIOINPUT_INTERRUPT_MODE { ... } (guest-selftest)",
    )
    failures += compare_enum_item_lists(
        driver_items=parse_enum_items(driver_int_mode_body, file=LOG_H, what="VIOINPUT_INTERRUPT_MODE"),
        tool_items=parse_enum_items(selftest_int_mode_body, file=GUEST_SELFTEST_CPP, what="VIOINPUT_INTERRUPT_MODE"),
        label="VIOINPUT_INTERRUPT_MODE (guest-selftest)",
    )

    driver_int_map_body = extract_struct_body(
        driver_text,
        r"\btypedef\s+enum\s+_VIOINPUT_INTERRUPT_MAPPING\s*\{",
        file=LOG_H,
        what="typedef enum _VIOINPUT_INTERRUPT_MAPPING { ... }",
    )
    tool_int_map_body = extract_struct_body(
        tool_text,
        r"\btypedef\s+enum\s+_VIOINPUT_INTERRUPT_MAPPING\s*\{",
        file=HIDTEST_C,
        what="typedef enum _VIOINPUT_INTERRUPT_MAPPING { ... } (hidtest)",
    )
    failures += compare_enum_item_lists(
        driver_items=parse_enum_items(driver_int_map_body, file=LOG_H, what="VIOINPUT_INTERRUPT_MAPPING"),
        tool_items=parse_enum_items(tool_int_map_body, file=HIDTEST_C, what="VIOINPUT_INTERRUPT_MAPPING"),
        label="VIOINPUT_INTERRUPT_MAPPING",
    )

    selftest_int_map_body = extract_struct_body(
        selftest_text,
        r"\benum\s+VIOINPUT_INTERRUPT_MAPPING\b[^{]*\{",
        file=GUEST_SELFTEST_CPP,
        what="enum VIOINPUT_INTERRUPT_MAPPING { ... } (guest-selftest)",
    )
    failures += compare_enum_item_lists(
        driver_items=parse_enum_items(driver_int_map_body, file=LOG_H, what="VIOINPUT_INTERRUPT_MAPPING"),
        tool_items=parse_enum_items(selftest_int_map_body, file=GUEST_SELFTEST_CPP, what="VIOINPUT_INTERRUPT_MAPPING"),
        label="VIOINPUT_INTERRUPT_MAPPING (guest-selftest)",
    )

    driver_vec_none = extract_define_int_literal(driver_text, "VIOINPUT_INTERRUPT_VECTOR_NONE", file=LOG_H)
    tool_vec_none = extract_define_int_literal(tool_text, "VIOINPUT_INTERRUPT_VECTOR_NONE", file=HIDTEST_C)
    if driver_vec_none != tool_vec_none:
        failures.append(
            "VIOINPUT_INTERRUPT_VECTOR_NONE mismatch: "
            f"driver=0x{driver_vec_none:X} hidtest=0x{tool_vec_none:X}"
        )

    selftest_vec_none = extract_const_int_literal(
        selftest_text, "VIOINPUT_INTERRUPT_VECTOR_NONE", file=GUEST_SELFTEST_CPP
    )
    if driver_vec_none != selftest_vec_none:
        failures.append(
            "VIOINPUT_INTERRUPT_VECTOR_NONE mismatch: "
            f"driver=0x{driver_vec_none:X} guest-selftest=0x{selftest_vec_none:X}"
        )

    # VIOINPUT_DEVICE_KIND is returned to user mode via VIOINPUT_STATE.DeviceKind.
    # hidtest duplicates the values for printing; keep them aligned with the driver enum.
    driver_kind_body = extract_struct_body(
        virtio_input_text,
        r"\btypedef\s+enum\s+_VIOINPUT_DEVICE_KIND\s*\{",
        file=VIRTIO_INPUT_H,
        what="typedef enum _VIOINPUT_DEVICE_KIND { ... }",
    )
    driver_kind_items = dict(parse_enum_items(driver_kind_body, file=VIRTIO_INPUT_H, what="VIOINPUT_DEVICE_KIND"))

    def require_kind(name: str) -> int:
        if name not in driver_kind_items:
            fail(f"{VIRTIO_INPUT_H.as_posix()}: expected enumerator {name} in VIOINPUT_DEVICE_KIND")
        return driver_kind_items[name]

    # Extract the hidtest constants directly (they are explicit assignments).
    kind_names = ("UNKNOWN", "KEYBOARD", "MOUSE", "TABLET")
    hidtest_kind_vals: dict[str, int] = {}
    for k in kind_names:
        m = re.search(rf"(?m)^\s*VIOINPUT_DEVICE_KIND_{k}\s*=\s*([-+]?(?:0x[0-9A-Fa-f]+|\d+))\s*,", tool_text)
        if not m:
            fail(f"{HIDTEST_C.as_posix()}: missing VIOINPUT_DEVICE_KIND_{k} enum entry")
        hidtest_kind_vals[k] = int(m.group(1), 0)

    unknown_driver = require_kind("VioInputDeviceKindUnknown")
    keyboard_driver = require_kind("VioInputDeviceKindKeyboard")
    mouse_driver = require_kind("VioInputDeviceKindMouse")
    tablet_driver = require_kind("VioInputDeviceKindTablet")
    if hidtest_kind_vals["UNKNOWN"] != unknown_driver:
        failures.append(
            f"VIOINPUT_DEVICE_KIND_UNKNOWN value mismatch: driver={unknown_driver} hidtest={hidtest_kind_vals['UNKNOWN']}"
        )
    if hidtest_kind_vals["KEYBOARD"] != keyboard_driver:
        failures.append(
            f"VIOINPUT_DEVICE_KIND_KEYBOARD value mismatch: driver={keyboard_driver} hidtest={hidtest_kind_vals['KEYBOARD']}"
        )
    if hidtest_kind_vals["MOUSE"] != mouse_driver:
        failures.append(
            f"VIOINPUT_DEVICE_KIND_MOUSE value mismatch: driver={mouse_driver} hidtest={hidtest_kind_vals['MOUSE']}"
        )
    if hidtest_kind_vals["TABLET"] != tablet_driver:
        failures.append(
            f"VIOINPUT_DEVICE_KIND_TABLET value mismatch: driver={tablet_driver} hidtest={hidtest_kind_vals['TABLET']}"
        )

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

    selftest_counters_body = extract_struct_body(
        selftest_text,
        r"\bstruct\s+VIOINPUT_COUNTERS\s*\{",
        file=GUEST_SELFTEST_CPP,
        what="struct VIOINPUT_COUNTERS { ... } (guest-selftest)",
    )
    selftest_counters_fields = extract_field_names(selftest_counters_body)
    failures += compare_field_lists(
        driver_fields=driver_counters_fields,
        tool_fields=selftest_counters_fields,
        label="VIOINPUT_COUNTERS (guest-selftest)",
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

    driver_interrupt_body = extract_struct_body(
        driver_text,
        r"\btypedef\s+struct\s+_VIOINPUT_INTERRUPT_INFO\s*\{",
        file=LOG_H,
        what="typedef struct _VIOINPUT_INTERRUPT_INFO { ... }",
    )
    tool_interrupt_body = extract_struct_body(
        tool_text,
        r"\btypedef\s+struct\s+_VIOINPUT_INTERRUPT_INFO\s*\{",
        file=HIDTEST_C,
        what="typedef struct _VIOINPUT_INTERRUPT_INFO { ... } (hidtest)",
    )

    driver_interrupt_fields = extract_field_names(driver_interrupt_body)
    tool_interrupt_fields = extract_field_names(tool_interrupt_body)
    failures += compare_field_lists(
        driver_fields=driver_interrupt_fields,
        tool_fields=tool_interrupt_fields,
        label="VIOINPUT_INTERRUPT_INFO",
    )

    selftest_interrupt_body = extract_struct_body(
        selftest_text,
        r"\bstruct\s+VIOINPUT_INTERRUPT_INFO\s*\{",
        file=GUEST_SELFTEST_CPP,
        what="struct VIOINPUT_INTERRUPT_INFO { ... } (guest-selftest)",
    )
    selftest_interrupt_fields = extract_field_names(selftest_interrupt_body)
    failures += compare_field_lists(
        driver_fields=driver_interrupt_fields,
        tool_fields=selftest_interrupt_fields,
        label="VIOINPUT_INTERRUPT_INFO (guest-selftest)",
    )

    if failures:
        print("error: Win7 virtio-input diagnostics ABI is out of sync:\n", file=sys.stderr)
        for line in failures:
            print(line, file=sys.stderr)
        print(
            "\nFix: keep these in sync:\n"
            "  - drivers/windows7/virtio-input/src/log.h\n"
            "  - drivers/windows7/virtio-input/src/virtio_input.h\n"
            "  - drivers/windows7/virtio-input/tools/hidtest/main.c\n"
            "  - drivers/windows7/tests/guest-selftest/src/main.cpp\n",
            file=sys.stderr,
        )
        return 1

    print(
        "ok: Win7 virtio-input diagnostics ABI is in sync "
        f"(counters v{driver_counters_version}, state v{driver_state_version}, interrupt v{driver_interrupt_version})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
