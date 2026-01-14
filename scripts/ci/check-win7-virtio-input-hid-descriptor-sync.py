#!/usr/bin/env python3
"""
CI guardrail: ensure Win7 virtio-input HID report descriptors stay in sync.

Why:
  - `drivers/windows7/virtio-input/src/descriptor.c` contains HID report descriptor
    byte arrays, and compile-time `C_ASSERT(sizeof(...) == N)` checks that are
    only validated when building with a Windows toolchain/WDK.
  - `drivers/windows7/virtio-input/tools/hidtest/main.c` also hard-codes expected
    report-descriptor lengths for runtime validation on Windows.

This script is Linux-runnable and detects drift between:
  - literal byte count in the report descriptor arrays (`0xNN` tokens)
  - the `C_ASSERT(sizeof(...) == N)` constants (validated only by Windows builds)
  - hidtest's `VIRTIO_INPUT_EXPECTED_*_REPORT_DESC_LEN` macros
  - (extra) report IDs + report sizes derived from parsing the HID report descriptor
    semantics (Report Size/Count + Input/Output items), compared against:
      - `drivers/windows7/virtio-input/src/hid_translate.h` report IDs/sizes
      - `drivers/windows7/virtio-input/tools/hidtest/main.c` expected caps lengths

Optional extra guardrail:
  - ensure hidtest's expected input report lengths match `HID_TRANSLATE_*_REPORT_SIZE`
    in `src/hid_translate.h`.
"""

from __future__ import annotations

from collections import defaultdict
import re
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]


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


def extract_hex_bytes(text: str) -> list[int]:
    return [int(b, 16) for b in re.findall(r"\b0x([0-9A-Fa-f]{2})\b", text)]


@dataclass(frozen=True)
class HidReportDescriptorParsed:
    # True if the descriptor uses report IDs at all. If true, HID report payloads
    # include an extra leading Report ID byte (as reflected by HidP_GetCaps).
    report_id_used: bool
    report_ids: set[int]
    input_bits_by_report_id: dict[int, int]
    output_bits_by_report_id: dict[int, int]
    feature_bits_by_report_id: dict[int, int]


def parse_hid_report_descriptor(data: list[int]) -> HidReportDescriptorParsed:
    """
    Minimal HID report descriptor parser sufficient for the virtio-input report
    descriptors in this repo.

    We track only the global items that affect report sizing:
      - Report Size (0x75)
      - Report Count (0x95)
      - Report ID (0x85)
    and the main items that contribute to report payload size:
      - Input (0x81)
      - Output (0x91)
      - Feature (0xB1)

    All other items are skipped.
    """

    input_bits: dict[int, int] = defaultdict(int)
    output_bits: dict[int, int] = defaultdict(int)
    feature_bits: dict[int, int] = defaultdict(int)

    report_ids: set[int] = set()
    report_id_used = False

    report_size_bits = 0
    report_count = 0
    current_report_id = 0
    global_stack: list[tuple[int, int, int]] = []

    i = 0
    while i < len(data):
        prefix = data[i]
        i += 1

        if prefix == 0xFE:
            # Long item: 0xFE, length, tag, <data...>
            if i + 2 > len(data):
                raise ValueError("truncated long item header")
            length = data[i]
            i += 1
            _tag = data[i]
            i += 1
            if i + length > len(data):
                raise ValueError("truncated long item data")
            i += length
            continue

        size_code = prefix & 0x03
        size = 4 if size_code == 3 else size_code  # 0,1,2,4 bytes
        type_code = (prefix >> 2) & 0x03
        tag = (prefix >> 4) & 0x0F

        if i + size > len(data):
            raise ValueError("truncated item data")
        raw = data[i : i + size]
        i += size

        value = int.from_bytes(bytes(raw), "little", signed=False) if size else 0

        # Global items (type=1).
        if type_code == 1 and tag == 7:  # Report Size
            report_size_bits = value
            continue
        if type_code == 1 and tag == 9:  # Report Count
            report_count = value
            continue
        if type_code == 1 and tag == 8:  # Report ID
            report_id_used = True
            current_report_id = value
            report_ids.add(current_report_id)
            continue
        if type_code == 1 and tag == 10:  # Push
            global_stack.append((report_size_bits, report_count, current_report_id))
            continue
        if type_code == 1 and tag == 11:  # Pop
            if not global_stack:
                raise ValueError("HID report descriptor: POP with empty global stack")
            report_size_bits, report_count, current_report_id = global_stack.pop()
            continue

        # Main items (type=0).
        if type_code == 0 and tag == 8:  # Input
            input_bits[current_report_id] += report_size_bits * report_count
            continue
        if type_code == 0 and tag == 9:  # Output
            output_bits[current_report_id] += report_size_bits * report_count
            continue
        if type_code == 0 and tag == 11:  # Feature
            feature_bits[current_report_id] += report_size_bits * report_count
            continue

    return HidReportDescriptorParsed(
        report_id_used=report_id_used,
        report_ids=report_ids,
        input_bits_by_report_id=dict(input_bits),
        output_bits_by_report_id=dict(output_bits),
        feature_bits_by_report_id=dict(feature_bits),
    )


def ceil_div(a: int, b: int) -> int:
    return (a + b - 1) // b


def report_len_bytes(bits: int, report_id_used: bool) -> int:
    if bits == 0:
        return 0
    n = ceil_div(bits, 8)
    if report_id_used:
        n += 1
    return n


def max_report_len_bytes(bits_by_report_id: dict[int, int], report_id_used: bool) -> int:
    if not bits_by_report_id:
        return 0
    max_bits = max(bits_by_report_id.values())
    return report_len_bytes(max_bits, report_id_used)


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


def extract_enum_int_literal(text: str, name: str) -> int:
    # Like extract_enum_int, but accepts both decimal and hex literals.
    m = re.search(
        rf"\b{re.escape(name)}\b\s*=\s*(0x[0-9A-Fa-f]+|\d+)\s*[uUlL]*\s*,",
        text,
    )
    if not m:
        raise ValueError(f"could not find '{name} = <int>,' in hid_translate.h")
    return int(m.group(1), 0)


def main() -> int:
    descriptor_c = REPO_ROOT / "drivers/windows7/virtio-input/src/descriptor.c"
    hidtest_c = REPO_ROOT / "drivers/windows7/virtio-input/tools/hidtest/main.c"
    hid_translate_h = REPO_ROOT / "drivers/windows7/virtio-input/src/hid_translate.h"

    missing = [p for p in (descriptor_c, hidtest_c, hid_translate_h) if not p.exists()]
    if missing:
        print("error: missing expected files:")
        for p in missing:
            try:
                p_rel = p.relative_to(REPO_ROOT)
            except ValueError:
                p_rel = p
            print(f"  - {p_rel}")
        return 2

    descriptor_text = descriptor_c.read_text(encoding="utf-8", errors="replace")
    hidtest_text = hidtest_c.read_text(encoding="utf-8", errors="replace")
    translate_text = hid_translate_h.read_text(encoding="utf-8", errors="replace")

    # Strip comments once up-front to make braced-initializer parsing robust against
    # braces inside comments.
    descriptor_text_no_comments = strip_c_comments(descriptor_text)

    failures: list[str] = []

    print("Win7 virtio-input HID descriptor sync check:")

    desc_bytes_by_kind: dict[str, list[int]] = {}

    for spec in SPECS:
        init = extract_braced_initializer(descriptor_text_no_comments, spec.array_name)
        desc_bytes_by_kind[spec.kind] = extract_hex_bytes(init)
        computed = count_hex_byte_literals(init)
        asserted = extract_c_assert_sizeof(descriptor_text_no_comments, spec.array_name)
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
        hidtest_consumer_input = extract_c_define_int(hidtest_text, "VIRTIO_INPUT_EXPECTED_CONSUMER_INPUT_LEN")
        hidtest_mouse_input = extract_c_define_int(hidtest_text, "VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN")
        hidtest_tablet_input = extract_c_define_int(hidtest_text, "VIRTIO_INPUT_EXPECTED_TABLET_INPUT_LEN")
        hidtest_kbd_output = extract_c_define_int(hidtest_text, "VIRTIO_INPUT_EXPECTED_KBD_OUTPUT_LEN")
        translate_kbd_size = extract_enum_int(translate_text, "HID_TRANSLATE_KEYBOARD_REPORT_SIZE")
        translate_mouse_size = extract_enum_int(translate_text, "HID_TRANSLATE_MOUSE_REPORT_SIZE")
        translate_tablet_size = extract_enum_int(translate_text, "HID_TRANSLATE_TABLET_REPORT_SIZE")
        translate_consumer_size = extract_enum_int(translate_text, "HID_TRANSLATE_CONSUMER_REPORT_SIZE")
        translate_kbd_id = extract_enum_int_literal(translate_text, "HID_TRANSLATE_REPORT_ID_KEYBOARD")
        translate_mouse_id = extract_enum_int_literal(translate_text, "HID_TRANSLATE_REPORT_ID_MOUSE")
        translate_consumer_id = extract_enum_int_literal(translate_text, "HID_TRANSLATE_REPORT_ID_CONSUMER")
        translate_tablet_id = extract_enum_int_literal(translate_text, "HID_TRANSLATE_REPORT_ID_TABLET")

        print(
            "  input report sizes:"
            f" kbd hidtest={hidtest_kbd_input} translate={translate_kbd_size},"
            f" consumer hidtest={hidtest_consumer_input} translate={translate_consumer_size},"
            f" mouse hidtest={hidtest_mouse_input} translate={translate_mouse_size},"
            f" tablet hidtest={hidtest_tablet_input} translate={translate_tablet_size}"
        )
        print(f"  keyboard output report size: hidtest={hidtest_kbd_output}")

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
        if hidtest_consumer_input != translate_consumer_size:
            failures.append(
                "\n".join(
                    [
                        "consumer input report length mismatch:",
                        f"  hidtest VIRTIO_INPUT_EXPECTED_CONSUMER_INPUT_LEN : {hidtest_consumer_input}",
                        f"  hid_translate.h HID_TRANSLATE_CONSUMER_REPORT_SIZE : {translate_consumer_size}",
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

        # Optional extra guardrail: parse the HID report descriptors and ensure
        # report IDs + report sizes match the translator/hidtest expectations.
        kbd_desc = parse_hid_report_descriptor(desc_bytes_by_kind["keyboard"])
        mouse_desc = parse_hid_report_descriptor(desc_bytes_by_kind["mouse"])
        tablet_desc = parse_hid_report_descriptor(desc_bytes_by_kind["tablet"])

        # Report ID sets.
        expected_kbd_ids = {translate_kbd_id, translate_consumer_id}
        expected_mouse_ids = {translate_mouse_id}
        expected_tablet_ids = {translate_tablet_id}

        if kbd_desc.report_ids != expected_kbd_ids:
            failures.append(
                "\n".join(
                    [
                        "keyboard descriptor report IDs mismatch:",
                        f"  descriptor report IDs : {sorted(kbd_desc.report_ids)}",
                        f"  expected (translate)  : {sorted(expected_kbd_ids)}",
                    ]
                )
            )
        if mouse_desc.report_ids != expected_mouse_ids:
            failures.append(
                "\n".join(
                    [
                        "mouse descriptor report IDs mismatch:",
                        f"  descriptor report IDs : {sorted(mouse_desc.report_ids)}",
                        f"  expected (translate)  : {sorted(expected_mouse_ids)}",
                    ]
                )
            )
        if tablet_desc.report_ids != expected_tablet_ids:
            failures.append(
                "\n".join(
                    [
                        "tablet descriptor report IDs mismatch:",
                        f"  descriptor report IDs : {sorted(tablet_desc.report_ids)}",
                        f"  expected (translate)  : {sorted(expected_tablet_ids)}",
                    ]
                )
            )

        # Derive max Input/Output lengths as reported by HidP_GetCaps.
        kbd_input_len_from_desc = max_report_len_bytes(kbd_desc.input_bits_by_report_id, kbd_desc.report_id_used)
        kbd_output_len_from_desc = max_report_len_bytes(kbd_desc.output_bits_by_report_id, kbd_desc.report_id_used)
        mouse_input_len_from_desc = max_report_len_bytes(mouse_desc.input_bits_by_report_id, mouse_desc.report_id_used)
        tablet_input_len_from_desc = max_report_len_bytes(
            tablet_desc.input_bits_by_report_id, tablet_desc.report_id_used
        )

        print(
            "  report sizes derived from descriptors:"
            f" kbd in={kbd_input_len_from_desc} out={kbd_output_len_from_desc},"
            f" mouse in={mouse_input_len_from_desc},"
            f" tablet in={tablet_input_len_from_desc}"
        )

        if kbd_input_len_from_desc != hidtest_kbd_input:
            failures.append(
                "\n".join(
                    [
                        "keyboard input report length mismatch (derived from report descriptor):",
                        f"  descriptor-derived max input len : {kbd_input_len_from_desc}",
                        f"  hidtest VIRTIO_INPUT_EXPECTED_KBD_INPUT_LEN : {hidtest_kbd_input}",
                    ]
                )
            )
        if kbd_output_len_from_desc != hidtest_kbd_output:
            failures.append(
                "\n".join(
                    [
                        "keyboard output report length mismatch (derived from report descriptor):",
                        f"  descriptor-derived max output len : {kbd_output_len_from_desc}",
                        f"  hidtest VIRTIO_INPUT_EXPECTED_KBD_OUTPUT_LEN : {hidtest_kbd_output}",
                    ]
                )
            )
        if mouse_input_len_from_desc != hidtest_mouse_input:
            failures.append(
                "\n".join(
                    [
                        "mouse input report length mismatch (derived from report descriptor):",
                        f"  descriptor-derived input len : {mouse_input_len_from_desc}",
                        f"  hidtest VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN : {hidtest_mouse_input}",
                    ]
                )
            )
        if tablet_input_len_from_desc != hidtest_tablet_input:
            failures.append(
                "\n".join(
                    [
                        "tablet input report length mismatch (derived from report descriptor):",
                        f"  descriptor-derived input len : {tablet_input_len_from_desc}",
                        f"  hidtest VIRTIO_INPUT_EXPECTED_TABLET_INPUT_LEN : {hidtest_tablet_input}",
                    ]
                )
            )

        # Per-report-ID input sizes (ensures consumer report doesn't drift).
        kbd_id_bits = kbd_desc.input_bits_by_report_id.get(translate_kbd_id, 0)
        consumer_id_bits = kbd_desc.input_bits_by_report_id.get(translate_consumer_id, 0)
        mouse_id_bits = mouse_desc.input_bits_by_report_id.get(translate_mouse_id, 0)
        tablet_id_bits = tablet_desc.input_bits_by_report_id.get(translate_tablet_id, 0)

        kbd_id_len = report_len_bytes(kbd_id_bits, kbd_desc.report_id_used)
        consumer_id_len = report_len_bytes(consumer_id_bits, kbd_desc.report_id_used)
        mouse_id_len = report_len_bytes(mouse_id_bits, mouse_desc.report_id_used)
        tablet_id_len = report_len_bytes(tablet_id_bits, tablet_desc.report_id_used)

        if kbd_id_len != translate_kbd_size:
            failures.append(
                "\n".join(
                    [
                        "keyboard Report ID input length mismatch (derived from report descriptor):",
                        f"  report id: {translate_kbd_id}",
                        f"  descriptor-derived len : {kbd_id_len}",
                        f"  hid_translate.h HID_TRANSLATE_KEYBOARD_REPORT_SIZE : {translate_kbd_size}",
                    ]
                )
            )
        if consumer_id_len != translate_consumer_size:
            failures.append(
                "\n".join(
                    [
                        "consumer Report ID input length mismatch (derived from report descriptor):",
                        f"  report id: {translate_consumer_id}",
                        f"  descriptor-derived len : {consumer_id_len}",
                        f"  hid_translate.h HID_TRANSLATE_CONSUMER_REPORT_SIZE : {translate_consumer_size}",
                    ]
                )
            )
        if consumer_id_len != hidtest_consumer_input:
            failures.append(
                "\n".join(
                    [
                        "consumer Report ID input length mismatch (hidtest macro vs descriptor-derived):",
                        f"  report id: {translate_consumer_id}",
                        f"  descriptor-derived len : {consumer_id_len}",
                        f"  hidtest VIRTIO_INPUT_EXPECTED_CONSUMER_INPUT_LEN : {hidtest_consumer_input}",
                    ]
                )
            )
        if mouse_id_len != translate_mouse_size:
            failures.append(
                "\n".join(
                    [
                        "mouse Report ID input length mismatch (derived from report descriptor):",
                        f"  report id: {translate_mouse_id}",
                        f"  descriptor-derived len : {mouse_id_len}",
                        f"  hid_translate.h HID_TRANSLATE_MOUSE_REPORT_SIZE : {translate_mouse_size}",
                    ]
                )
            )
        if tablet_id_len != translate_tablet_size:
            failures.append(
                "\n".join(
                    [
                        "tablet Report ID input length mismatch (derived from report descriptor):",
                        f"  report id: {translate_tablet_id}",
                        f"  descriptor-derived len : {tablet_id_len}",
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
    try:
        raise SystemExit(main())
    except ValueError as e:
        # Keep failures readable in CI logs (no stack trace for pattern mismatch).
        print(f"error: {e}")
        raise SystemExit(2)
