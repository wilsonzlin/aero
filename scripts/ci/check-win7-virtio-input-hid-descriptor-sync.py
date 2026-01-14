#!/usr/bin/env python3
"""
CI guardrail: ensure Win7 virtio-input HID descriptor lengths stay in sync.

Why:
  - `drivers/windows7/virtio-input/src/descriptor.c` contains HID report descriptor
    byte arrays, and compile-time `C_ASSERT(sizeof(...) == N)` checks that are
    only validated when building with a Windows toolchain/WDK.
  - `drivers/windows7/virtio-input/tools/hidtest/main.c` also hard-codes expected
    report-descriptor lengths for runtime validation on Windows.

This script is Linux-runnable and detects drift between:
  - literal byte count in the report descriptor arrays
  - the `C_ASSERT(sizeof(...) == N)` constants
  - hidtest's `VIRTIO_INPUT_EXPECTED_*_REPORT_DESC_LEN` macros

Optional extra guardrail:
  - ensure hidtest's expected input report lengths match `HID_TRANSLATE_*_REPORT_SIZE`
    in `src/hid_translate.h`.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class DescriptorSpec:
    kind: str
    array_name: str
    hidtest_len_macro: str


SPECS: list[DescriptorSpec] = [
    DescriptorSpec(
        kind="keyboard",
        array_name="VirtioInputKeyboardReportDescriptor",
        hidtest_len_macro="VIRTIO_INPUT_EXPECTED_KBD_REPORT_DESC_LEN",
    ),
    DescriptorSpec(
        kind="mouse",
        array_name="VirtioInputMouseReportDescriptor",
        hidtest_len_macro="VIRTIO_INPUT_EXPECTED_MOUSE_REPORT_DESC_LEN",
    ),
    DescriptorSpec(
        kind="tablet",
        array_name="VirtioInputTabletReportDescriptor",
        hidtest_len_macro="VIRTIO_INPUT_EXPECTED_TABLET_REPORT_DESC_LEN",
    ),
]


def strip_c_comments(text: str) -> str:
    # Remove /* ... */ first so we don't accidentally kill // inside a block comment.
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    # Remove // ... comments.
    text = re.sub(r"//.*?$", "", text, flags=re.M)
    return text


def extract_braced_initializer(text: str, symbol: str) -> str:
    """
    Extract the contents of the braced initializer for:
        const UCHAR <symbol>[] = { ... };

    Returns the substring inside the outermost braces.
    """
    # Be permissive about whitespace and qualifiers; only anchor on the variable name
    # and the first '{' after the assignment.
    # Allow both `name[] = { ... }` and `name[N] = { ... }` forms.
    m = re.search(rf"\b{re.escape(symbol)}\b\s*\[\s*(?:\d+\s*)?\]\s*=\s*\{{", text)
    if not m:
        raise ValueError(f"could not find initializer for '{symbol}[] = {{ ... }}'")

    # Start scanning right after the first '{'.
    i = m.end() - 1
    assert text[i] == "{"

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

    raise ValueError(f"unterminated initializer for '{symbol}'")


def count_hex_byte_literals(text: str) -> int:
    """
    Count 0xNN tokens.

    We intentionally only count canonical byte tokens (0x00..0xFF) as requested,
    since the HID report descriptors in this driver are expressed in that form.
    """
    return len(re.findall(r"\b0x[0-9A-Fa-f]{2}\b", text))


def extract_c_assert_sizeof(text: str, symbol: str) -> int:
    m = re.search(
        rf"C_ASSERT\s*\(\s*sizeof\s*\(\s*{re.escape(symbol)}\s*\)\s*==\s*(\d+)\s*[uUlL]*\s*\)\s*;",
        text,
        flags=re.S,
    )
    if not m:
        raise ValueError(f"could not find C_ASSERT(sizeof({symbol}) == N) in descriptor.c")
    return int(m.group(1))


def extract_c_define_int(text: str, macro: str) -> int:
    m = re.search(rf"^\s*#define\s+{re.escape(macro)}\s+(\d+)\s*[uUlL]*\b", text, flags=re.M)
    if not m:
        raise ValueError(f"could not find '#define {macro} N' in hidtest/main.c")
    return int(m.group(1))


def extract_enum_int(text: str, name: str) -> int:
    # Match `NAME = 123,` inside an enum.
    m = re.search(rf"\b{re.escape(name)}\b\s*=\s*(\d+)\s*[uUlL]*\s*,", text)
    if not m:
        raise ValueError(f"could not find '{name} = N,' in hid_translate.h")
    return int(m.group(1))


def main() -> int:
    descriptor_c = Path("drivers/windows7/virtio-input/src/descriptor.c")
    hidtest_c = Path("drivers/windows7/virtio-input/tools/hidtest/main.c")
    hid_translate_h = Path("drivers/windows7/virtio-input/src/hid_translate.h")

    missing = [p for p in (descriptor_c, hidtest_c, hid_translate_h) if not p.exists()]
    if missing:
        print("error: missing expected files:")
        for p in missing:
            print(f"  - {p}")
        return 2

    descriptor_text = descriptor_c.read_text(encoding="utf-8", errors="replace")
    hidtest_text = hidtest_c.read_text(encoding="utf-8", errors="replace")
    translate_text = hid_translate_h.read_text(encoding="utf-8", errors="replace")

    failures: list[str] = []

    print("Win7 virtio-input HID descriptor sync check:")

    for spec in SPECS:
        init = extract_braced_initializer(descriptor_text, spec.array_name)
        init = strip_c_comments(init)
        computed = count_hex_byte_literals(init)
        asserted = extract_c_assert_sizeof(descriptor_text, spec.array_name)
        expected = extract_c_define_int(hidtest_text, spec.hidtest_len_macro)

        print(f"  {spec.kind}: computed={computed} C_ASSERT={asserted} hidtest={expected}")

        if computed != asserted or computed != expected:
            failures.append(
                "\n".join(
                    [
                        f"{spec.kind} descriptor length mismatch:",
                        f"  computed (literal 0xNN bytes) : {computed}",
                        f"  descriptor.c C_ASSERT          : {asserted}",
                        f"  hidtest expected macro         : {expected} ({spec.hidtest_len_macro})",
                    ]
                )
            )

    # Optional extra guardrail: expected input report lengths.
    try:
        hidtest_kbd_input = extract_c_define_int(hidtest_text, "VIRTIO_INPUT_EXPECTED_KBD_INPUT_LEN")
        hidtest_mouse_input = extract_c_define_int(hidtest_text, "VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN")
        hidtest_tablet_input = extract_c_define_int(hidtest_text, "VIRTIO_INPUT_EXPECTED_TABLET_INPUT_LEN")
        translate_kbd_size = extract_enum_int(translate_text, "HID_TRANSLATE_KEYBOARD_REPORT_SIZE")
        translate_mouse_size = extract_enum_int(translate_text, "HID_TRANSLATE_MOUSE_REPORT_SIZE")
        translate_tablet_size = extract_enum_int(translate_text, "HID_TRANSLATE_TABLET_REPORT_SIZE")

        print(
            "  input report sizes:"
            f" kbd hidtest={hidtest_kbd_input} translate={translate_kbd_size},"
            f" mouse hidtest={hidtest_mouse_input} translate={translate_mouse_size},"
            f" tablet hidtest={hidtest_tablet_input} translate={translate_tablet_size}"
        )

        if hidtest_kbd_input != translate_kbd_size:
            failures.append(
                "\n".join(
                    [
                        "keyboard input report length mismatch:",
                        f"  hidtest VIRTIO_INPUT_EXPECTED_KBD_INPUT_LEN : {hidtest_kbd_input}",
                        f"  hid_translate.h HID_TRANSLATE_KEYBOARD_REPORT_SIZE : {translate_kbd_size}",
                    ]
                )
            )
        if hidtest_mouse_input != translate_mouse_size:
            failures.append(
                "\n".join(
                    [
                        "mouse input report length mismatch:",
                        f"  hidtest VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN : {hidtest_mouse_input}",
                        f"  hid_translate.h HID_TRANSLATE_MOUSE_REPORT_SIZE : {translate_mouse_size}",
                    ]
                )
            )
        if hidtest_tablet_input != translate_tablet_size:
            failures.append(
                "\n".join(
                    [
                        "tablet input report length mismatch:",
                        f"  hidtest VIRTIO_INPUT_EXPECTED_TABLET_INPUT_LEN : {hidtest_tablet_input}",
                        f"  hid_translate.h HID_TRANSLATE_TABLET_REPORT_SIZE : {translate_tablet_size}",
                    ]
                )
            )
    except ValueError as e:
        # Don't fail if the optional fields are missing; the primary sync check is
        # still valuable, and this script should remain lightweight.
        print(f"  note: skipped input report size cross-check: {e}")

    if failures:
        print("\nerror: virtio-input HID descriptor definitions are out of sync:\n")
        for msg in failures:
            print(msg)
            print()
        print(
            "Fix: update the report descriptor byte arrays and keep these in sync:\n"
            "  - descriptor.c: C_ASSERT(sizeof(...ReportDescriptor) == N)\n"
            "  - hidtest/main.c: VIRTIO_INPUT_EXPECTED_*_REPORT_DESC_LEN\n"
        )
        return 1

    print("ok: virtio-input HID descriptor lengths are in sync")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
