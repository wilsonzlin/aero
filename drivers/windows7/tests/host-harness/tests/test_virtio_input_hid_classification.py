#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from dataclasses import dataclass
from pathlib import Path


@dataclass
class HidReportDescriptorSummary:
    keyboard_app_collections: int = 0
    mouse_app_collections: int = 0
    tablet_app_collections: int = 0
    mouse_xy_relative_collections: int = 0
    mouse_xy_absolute_collections: int = 0


def _extract_uchar_array_bytes(source: str, name: str) -> bytes:
    """
    Extract a `const UCHAR <name>[] = { ... };` initializer from a C file and return it as bytes.

    This is intentionally lightweight (regex-based) since it is only used for the in-tree
    virtio-input descriptor fixtures and runs in CI.
    """

    m = re.search(
        rf"const\s+UCHAR\s+{re.escape(name)}\s*\[\s*\]\s*=\s*\{{(.*?)\}};",
        source,
        flags=re.DOTALL,
    )
    if not m:
        raise AssertionError(f"failed to find UCHAR array {name}")
    body = m.group(1)

    # Strip comments so numeric parsing doesn't accidentally match comment text.
    body = re.sub(r"//.*", "", body)
    body = re.sub(r"/\*.*?\*/", "", body, flags=re.DOTALL)

    hex_nums = re.findall(r"0x[0-9A-Fa-f]+", body)
    if not hex_nums:
        raise AssertionError(f"no hex byte literals found in {name}")

    out = bytearray()
    for tok in hex_nums:
        v = int(tok, 16)
        if v < 0 or v > 0xFF:
            raise AssertionError(f"byte out of range in {name}: {tok}")
        out.append(v)
    return bytes(out)


def summarize_hid_report_descriptor(desc: bytes) -> HidReportDescriptorSummary:
    """
    Python port of the guest selftest's HID report descriptor summary logic.

    This test exists to prevent regressions where the in-tree virtio-input tablet
    descriptor would be treated as an "unknown" or "mouse" device, causing the
    virtio-input selftest to FAIL when a tablet HID function is present.
    """

    out = HidReportDescriptorSummary()

    usage_page = 0
    usage_page_stack: list[int] = []
    local_usages: list[int] = []
    local_usage_min: int | None = None
    local_usage_max: int | None = None

    class Kind:
        UNKNOWN = 0
        KEYBOARD = 1
        MOUSE_OR_POINTER = 2
        TABLET = 3

    @dataclass
    class CollectionCtx:
        is_application: bool = False
        usage_page: int = 0
        usage: int = 0
        kind: int = Kind.UNKNOWN

        saw_x_abs: bool = False
        saw_y_abs: bool = False
        saw_x_rel: bool = False
        saw_y_rel: bool = False

    collection_stack: list[CollectionCtx] = []

    def clear_locals() -> None:
        local_usages.clear()
        nonlocal local_usage_min, local_usage_max
        local_usage_min = None
        local_usage_max = None

    def local_usage_includes(u: int) -> bool:
        if u in local_usages:
            return True
        if local_usage_min is not None and local_usage_max is not None:
            return local_usage_min <= u <= local_usage_max
        if local_usage_min is not None and local_usage_max is None:
            # Best-effort: some descriptors use a single Usage Minimum without a matching maximum.
            return local_usage_min == u
        return False

    def finalize_collection(ctx: CollectionCtx) -> None:
        if not ctx.is_application:
            return
        if ctx.kind == Kind.KEYBOARD:
            out.keyboard_app_collections += 1
            return
        if ctx.kind == Kind.TABLET:
            out.tablet_app_collections += 1
            return
        if ctx.kind == Kind.MOUSE_OR_POINTER:
            has_rel_xy = ctx.saw_x_rel and ctx.saw_y_rel
            abs_only = (ctx.saw_x_abs and ctx.saw_y_abs) and not (ctx.saw_x_rel or ctx.saw_y_rel)

            if has_rel_xy:
                out.mouse_xy_relative_collections += 1
            if abs_only:
                out.mouse_xy_absolute_collections += 1

            # Keep this logic in sync with the guest selftest:
            # absolute-only pointers are treated as tablet-like devices.
            if abs_only:
                out.tablet_app_collections += 1
            else:
                out.mouse_app_collections += 1

    i = 0
    while i < len(desc):
        prefix = desc[i]
        i += 1

        if prefix == 0xFE:
            # Long item: 0xFE, size, tag, data...
            if i + 2 > len(desc):
                break
            size = desc[i]
            i += 1
            i += 1  # long item tag (ignored)
            if i + size > len(desc):
                break
            i += size
            continue

        size_code = prefix & 0x3
        typ = (prefix >> 2) & 0x3
        tag = (prefix >> 4) & 0xF

        data_size = 4 if size_code == 3 else size_code
        if i + data_size > len(desc):
            break

        value = 0
        for j in range(data_size):
            value |= desc[i + j] << (8 * j)
        i += data_size

        if typ == 0:  # Main
            if tag == 0xA:  # Collection
                collection_type = value & 0xFF
                usage: int | None = None
                if local_usages:
                    usage = local_usages[0]
                elif local_usage_min is not None:
                    usage = local_usage_min

                ctx = CollectionCtx()
                ctx.is_application = collection_type == 0x01
                ctx.usage_page = usage_page
                ctx.usage = usage or 0

                if ctx.is_application:
                    # Generic Desktop: Keyboard(0x06), Mouse(0x02), Pointer(0x01)
                    if ctx.usage_page == 0x01 and ctx.usage == 0x06:
                        ctx.kind = Kind.KEYBOARD
                    elif ctx.usage_page == 0x01 and ctx.usage in (0x01, 0x02):
                        ctx.kind = Kind.MOUSE_OR_POINTER
                    elif ctx.usage_page == 0x0D:
                        ctx.kind = Kind.TABLET

                collection_stack.append(ctx)

            elif tag == 0xC:  # End Collection
                if collection_stack:
                    ctx = collection_stack.pop()
                    finalize_collection(ctx)

            elif tag == 0x8:  # Input
                # Input flags:
                #   bit0: Data(0) / Constant(1)
                #   bit1: Array(0) / Variable(1)
                #   bit2: Absolute(0) / Relative(1)
                is_data = (value & 0x01) == 0
                is_var = (value & 0x02) != 0
                is_relative = (value & 0x04) != 0

                if is_data and is_var:
                    has_x = (usage_page == 0x01) and local_usage_includes(0x30)
                    has_y = (usage_page == 0x01) and local_usage_includes(0x31)
                    if has_x or has_y:
                        for ctx in reversed(collection_stack):
                            if not ctx.is_application:
                                continue
                            if ctx.kind != Kind.MOUSE_OR_POINTER:
                                break
                            if has_x:
                                if is_relative:
                                    ctx.saw_x_rel = True
                                else:
                                    ctx.saw_x_abs = True
                            if has_y:
                                if is_relative:
                                    ctx.saw_y_rel = True
                                else:
                                    ctx.saw_y_abs = True
                            break

            # Local items are cleared after each main item per HID spec.
            clear_locals()

        elif typ == 1:  # Global
            if tag == 0x0:  # Usage Page
                usage_page = value
            elif tag == 0xA:  # Push
                usage_page_stack.append(usage_page)
            elif tag == 0xB:  # Pop
                if usage_page_stack:
                    usage_page = usage_page_stack.pop()

        elif typ == 2:  # Local
            if tag == 0x0:  # Usage
                local_usages.append(value)
            elif tag == 0x1:  # Usage Minimum
                local_usage_min = value
            elif tag == 0x2:  # Usage Maximum
                local_usage_max = value

    # Best-effort: close any unterminated collections so we still compute classification.
    while collection_stack:
        finalize_collection(collection_stack.pop())

    return out


def classify_descriptor(summary: HidReportDescriptorSummary) -> str:
    """
    Mirror the guest selftest's per-device classification logic.
    """

    has_keyboard = summary.keyboard_app_collections > 0
    has_mouse = summary.mouse_xy_relative_collections > 0
    has_tablet = summary.tablet_app_collections > 0

    has_unclassified_mouse_collections = summary.mouse_app_collections > summary.mouse_xy_relative_collections
    kind_count = int(has_keyboard) + int(has_mouse) + int(has_tablet)

    if has_unclassified_mouse_collections:
        return "unknown"
    if kind_count > 1:
        return "ambiguous"
    if has_keyboard:
        return "keyboard"
    if has_mouse:
        return "mouse"
    if has_tablet:
        return "tablet"
    return "unknown"


class VirtioInputHidClassificationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        repo_root = Path(__file__).resolve().parents[5]
        desc_path = repo_root / "drivers" / "windows7" / "virtio-input" / "src" / "descriptor.c"
        text = desc_path.read_text(encoding="utf-8", errors="replace")

        cls.keyboard_desc = _extract_uchar_array_bytes(text, "VirtioInputKeyboardReportDescriptor")
        cls.mouse_desc = _extract_uchar_array_bytes(text, "VirtioInputMouseReportDescriptor")
        cls.tablet_desc = _extract_uchar_array_bytes(text, "VirtioInputTabletReportDescriptor")

    def test_keyboard_descriptor_classifies_as_keyboard(self) -> None:
        summary = summarize_hid_report_descriptor(self.keyboard_desc)
        self.assertGreaterEqual(summary.keyboard_app_collections, 1)
        self.assertEqual(classify_descriptor(summary), "keyboard")

    def test_mouse_descriptor_classifies_as_relative_mouse(self) -> None:
        summary = summarize_hid_report_descriptor(self.mouse_desc)
        self.assertGreaterEqual(summary.mouse_xy_relative_collections, 1)
        self.assertEqual(summary.tablet_app_collections, 0)
        self.assertEqual(classify_descriptor(summary), "mouse")

    def test_tablet_descriptor_classifies_as_tablet(self) -> None:
        summary = summarize_hid_report_descriptor(self.tablet_desc)
        self.assertGreaterEqual(summary.tablet_app_collections, 1)
        self.assertEqual(summary.mouse_xy_relative_collections, 0)
        self.assertGreaterEqual(summary.mouse_xy_absolute_collections, 1)
        self.assertEqual(classify_descriptor(summary), "tablet")

    def test_digitizer_descriptor_classifies_as_tablet(self) -> None:
        # Minimal Digitizers (0x0D) Application Collection with absolute X/Y.
        digitizer_desc = bytes(
            [
                0x05,
                0x0D,  # Usage Page (Digitizers)
                0x09,
                0x04,  # Usage (Touch Screen)
                0xA1,
                0x01,  # Collection (Application)
                0x05,
                0x01,  # Usage Page (Generic Desktop)
                0x09,
                0x30,  # Usage (X)
                0x09,
                0x31,  # Usage (Y)
                0x15,
                0x00,  # Logical Minimum (0)
                0x26,
                0xFF,
                0x7F,  # Logical Maximum (32767)
                0x75,
                0x10,  # Report Size (16)
                0x95,
                0x02,  # Report Count (2)
                0x81,
                0x02,  # Input (Data,Var,Abs)
                0xC0,  # End Collection
            ]
        )
        summary = summarize_hid_report_descriptor(digitizer_desc)
        self.assertGreaterEqual(summary.tablet_app_collections, 1)
        self.assertEqual(classify_descriptor(summary), "tablet")

    def test_pointer_application_with_absolute_xy_classifies_as_tablet(self) -> None:
        # Minimal Generic Desktop Pointer application collection with absolute X/Y.
        pointer_abs_desc = bytes(
            [
                0x05,
                0x01,  # Usage Page (Generic Desktop)
                0x09,
                0x01,  # Usage (Pointer)
                0xA1,
                0x01,  # Collection (Application)
                0x09,
                0x30,  # Usage (X)
                0x09,
                0x31,  # Usage (Y)
                0x15,
                0x00,  # Logical Minimum (0)
                0x26,
                0xFF,
                0x7F,  # Logical Maximum (32767)
                0x75,
                0x10,  # Report Size (16)
                0x95,
                0x02,  # Report Count (2)
                0x81,
                0x02,  # Input (Data,Var,Abs)
                0xC0,  # End Collection
            ]
        )
        summary = summarize_hid_report_descriptor(pointer_abs_desc)
        self.assertGreaterEqual(summary.tablet_app_collections, 1)
        self.assertEqual(summary.mouse_xy_relative_collections, 0)
        self.assertEqual(classify_descriptor(summary), "tablet")

    def test_mouse_descriptor_without_xy_is_unknown(self) -> None:
        # Minimal mouse application collection that lacks X/Y; should not be treated as a valid
        # relative mouse device by the guest selftest logic.
        button_only_mouse = bytes(
            [
                0x05,
                0x01,  # Usage Page (Generic Desktop)
                0x09,
                0x02,  # Usage (Mouse)
                0xA1,
                0x01,  # Collection (Application)
                0x05,
                0x09,  # Usage Page (Button)
                0x19,
                0x01,  # Usage Minimum (Button 1)
                0x29,
                0x03,  # Usage Maximum (Button 3)
                0x15,
                0x00,  # Logical Minimum (0)
                0x25,
                0x01,  # Logical Maximum (1)
                0x95,
                0x03,  # Report Count (3)
                0x75,
                0x01,  # Report Size (1)
                0x81,
                0x02,  # Input (Data,Var,Abs) ; Buttons only (no X/Y)
                0xC0,  # End Collection
            ]
        )
        summary = summarize_hid_report_descriptor(button_only_mouse)
        self.assertEqual(summary.mouse_app_collections, 1)
        self.assertEqual(summary.mouse_xy_relative_collections, 0)
        self.assertEqual(classify_descriptor(summary), "unknown")

    def test_mouse_xy_usage_range_relative_classifies_as_mouse(self) -> None:
        # Some HID descriptors use Usage Min/Max for X/Y instead of individual Usage items.
        mouse_xy_range = bytes(
            [
                0x05,
                0x01,  # Usage Page (Generic Desktop)
                0x09,
                0x02,  # Usage (Mouse)
                0xA1,
                0x01,  # Collection (Application)
                0x05,
                0x01,  # Usage Page (Generic Desktop)
                0x19,
                0x30,  # Usage Minimum (X)
                0x29,
                0x31,  # Usage Maximum (Y)
                0x15,
                0x81,  # Logical Minimum (-127)
                0x25,
                0x7F,  # Logical Maximum (127)
                0x75,
                0x08,  # Report Size (8)
                0x95,
                0x02,  # Report Count (2)
                0x81,
                0x06,  # Input (Data,Var,Rel)
                0xC0,  # End Collection
            ]
        )
        summary = summarize_hid_report_descriptor(mouse_xy_range)
        self.assertEqual(summary.mouse_xy_relative_collections, 1)
        self.assertEqual(summary.tablet_app_collections, 0)
        self.assertEqual(classify_descriptor(summary), "mouse")

    def test_mouse_xy_usage_range_absolute_classifies_as_tablet(self) -> None:
        mouse_xy_range_abs = bytes(
            [
                0x05,
                0x01,  # Usage Page (Generic Desktop)
                0x09,
                0x02,  # Usage (Mouse)
                0xA1,
                0x01,  # Collection (Application)
                0x05,
                0x01,  # Usage Page (Generic Desktop)
                0x19,
                0x30,  # Usage Minimum (X)
                0x29,
                0x31,  # Usage Maximum (Y)
                0x15,
                0x00,  # Logical Minimum (0)
                0x26,
                0xFF,
                0x7F,  # Logical Maximum (32767)
                0x75,
                0x10,  # Report Size (16)
                0x95,
                0x02,  # Report Count (2)
                0x81,
                0x02,  # Input (Data,Var,Abs)
                0xC0,  # End Collection
            ]
        )
        summary = summarize_hid_report_descriptor(mouse_xy_range_abs)
        self.assertEqual(summary.mouse_xy_relative_collections, 0)
        self.assertGreaterEqual(summary.mouse_xy_absolute_collections, 1)
        self.assertGreaterEqual(summary.tablet_app_collections, 1)
        self.assertEqual(classify_descriptor(summary), "tablet")

    def test_mouse_with_only_one_axis_is_unknown(self) -> None:
        # A pointing device that exposes only X should not be accepted as a valid relative mouse device.
        x_only = bytes(
            [
                0x05,
                0x01,  # Usage Page (Generic Desktop)
                0x09,
                0x02,  # Usage (Mouse)
                0xA1,
                0x01,  # Collection (Application)
                0x05,
                0x01,  # Usage Page (Generic Desktop)
                0x19,
                0x30,  # Usage Minimum (X)
                0x29,
                0x30,  # Usage Maximum (X)
                0x15,
                0x81,  # Logical Minimum (-127)
                0x25,
                0x7F,  # Logical Maximum (127)
                0x75,
                0x08,  # Report Size (8)
                0x95,
                0x01,  # Report Count (1)
                0x81,
                0x06,  # Input (Data,Var,Rel)
                0xC0,  # End Collection
            ]
        )
        summary = summarize_hid_report_descriptor(x_only)
        self.assertEqual(summary.mouse_app_collections, 1)
        self.assertEqual(summary.mouse_xy_relative_collections, 0)
        self.assertEqual(classify_descriptor(summary), "unknown")

    def test_keyboard_and_mouse_in_one_descriptor_is_ambiguous(self) -> None:
        # Minimal two-application descriptor: Keyboard + Mouse. The guest selftest treats this as an
        # ambiguous device (contract expects separate keyboard+mouse devices).
        keyboard_plus_mouse = bytes(
            [
                # Keyboard application
                0x05,
                0x01,  # Usage Page (Generic Desktop)
                0x09,
                0x06,  # Usage (Keyboard)
                0xA1,
                0x01,  # Collection (Application)
                0xC0,  # End Collection
                # Mouse application with relative X/Y
                0x05,
                0x01,  # Usage Page (Generic Desktop)
                0x09,
                0x02,  # Usage (Mouse)
                0xA1,
                0x01,  # Collection (Application)
                0x09,
                0x30,  # Usage (X)
                0x09,
                0x31,  # Usage (Y)
                0x15,
                0x81,  # Logical Minimum (-127)
                0x25,
                0x7F,  # Logical Maximum (127)
                0x75,
                0x08,  # Report Size (8)
                0x95,
                0x02,  # Report Count (2)
                0x81,
                0x06,  # Input (Data,Var,Rel)
                0xC0,  # End Collection
            ]
        )
        summary = summarize_hid_report_descriptor(keyboard_plus_mouse)
        self.assertGreaterEqual(summary.keyboard_app_collections, 1)
        self.assertGreaterEqual(summary.mouse_xy_relative_collections, 1)
        self.assertEqual(classify_descriptor(summary), "ambiguous")

    def test_keyboard_and_tablet_in_one_descriptor_is_ambiguous(self) -> None:
        keyboard_plus_tablet = bytes(
            [
                # Keyboard application
                0x05,
                0x01,  # Usage Page (Generic Desktop)
                0x09,
                0x06,  # Usage (Keyboard)
                0xA1,
                0x01,  # Collection (Application)
                0xC0,  # End Collection
                # Digitizer / touchscreen application
                0x05,
                0x0D,  # Usage Page (Digitizers)
                0x09,
                0x04,  # Usage (Touch Screen)
                0xA1,
                0x01,  # Collection (Application)
                0xC0,  # End Collection
            ]
        )
        summary = summarize_hid_report_descriptor(keyboard_plus_tablet)
        self.assertGreaterEqual(summary.keyboard_app_collections, 1)
        self.assertGreaterEqual(summary.tablet_app_collections, 1)
        self.assertEqual(classify_descriptor(summary), "ambiguous")

    def test_mouse_and_tablet_in_one_descriptor_is_ambiguous(self) -> None:
        mouse_plus_tablet = bytes(
            [
                # Mouse application with relative X/Y
                0x05,
                0x01,  # Usage Page (Generic Desktop)
                0x09,
                0x02,  # Usage (Mouse)
                0xA1,
                0x01,  # Collection (Application)
                0x09,
                0x30,  # Usage (X)
                0x09,
                0x31,  # Usage (Y)
                0x15,
                0x81,  # Logical Minimum (-127)
                0x25,
                0x7F,  # Logical Maximum (127)
                0x75,
                0x08,  # Report Size (8)
                0x95,
                0x02,  # Report Count (2)
                0x81,
                0x06,  # Input (Data,Var,Rel)
                0xC0,  # End Collection
                # Digitizer / touchscreen application
                0x05,
                0x0D,  # Usage Page (Digitizers)
                0x09,
                0x04,  # Usage (Touch Screen)
                0xA1,
                0x01,  # Collection (Application)
                0xC0,  # End Collection
            ]
        )
        summary = summarize_hid_report_descriptor(mouse_plus_tablet)
        self.assertGreaterEqual(summary.mouse_xy_relative_collections, 1)
        self.assertGreaterEqual(summary.tablet_app_collections, 1)
        self.assertEqual(classify_descriptor(summary), "ambiguous")


if __name__ == "__main__":
    unittest.main()
