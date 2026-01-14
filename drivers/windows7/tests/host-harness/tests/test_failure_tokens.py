#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import importlib.util
import re
import sys
import unittest
from pathlib import Path


def _load_harness():
    harness_path = Path(__file__).resolve().parents[1] / "invoke_aero_virtio_win7_tests.py"
    spec = importlib.util.spec_from_file_location("invoke_aero_virtio_win7_tests", harness_path)
    assert spec and spec.loader

    module = importlib.util.module_from_spec(spec)
    # Register the module before execution so dataclasses can resolve __module__ correctly.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


_TOKEN_RE = re.compile(r"^FAIL: [A-Z0-9_]+:")


class FailureTokenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_virtio_snd_skip_tokens(self) -> None:
        h = self.harness

        msg = h._virtio_snd_skip_failure_message(b"virtio-snd: skipped (enable with --test-snd)\n")
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_SKIPPED:"))

        msg = h._virtio_snd_skip_failure_message(b"virtio-snd: disabled by --disable-snd\n")
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_SKIPPED:"))

        msg = h._virtio_snd_skip_failure_message(b"virtio-snd: pci device not detected\n")
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_SKIPPED:"))

    def test_virtio_snd_skipped_token_includes_irq_fields_when_present(self) -> None:
        h = self.harness

        # Simulate tail truncation: the serial rolling tail no longer contains the marker line or
        # the human reason string, but we did capture them incrementally during parsing.
        msg = h._virtio_snd_skip_failure_message(
            b"",
            marker_line="AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP|irq_mode=msix|irq_message_count=3",
            skip_reason="guest_not_configured_with_--test-snd",
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_SKIPPED:"))
        self.assertIn("irq_mode=msix", msg)
        self.assertIn("irq_message_count=3", msg)
        self.assertIn("--test-snd", msg)

    def test_virtio_snd_failed_token_includes_reason_and_irq_fields_when_present(self) -> None:
        h = self.harness

        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|wrong_service|irq_mode=msix|irq_message_count=3\n"
        msg = h._virtio_snd_fail_failure_message(tail)
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_FAILED:"))
        self.assertIn("reason=wrong_service", msg)
        self.assertIn("irq_mode=msix", msg)
        self.assertIn("irq_message_count=3", msg)

    def test_virtio_snd_capture_failed_token_includes_reason_when_present(self) -> None:
        h = self.harness

        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|endpoint_missing\n"
        msg = h._virtio_snd_capture_fail_failure_message(tail)
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_CAPTURE_FAILED:"))
        self.assertIn("reason=endpoint_missing", msg)

    def test_virtio_snd_duplex_failed_token_includes_reason_and_hr_when_present(self) -> None:
        h = self.harness

        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|FAIL|reason=no_matching_endpoint|hr=0x80004005\n"
        msg = h._virtio_snd_duplex_fail_failure_message(tail)
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_DUPLEX_FAILED:"))
        self.assertIn("reason=no_matching_endpoint", msg)
        self.assertIn("hr=0x80004005", msg)

    def test_virtio_snd_buffer_limits_failed_token_includes_reason_and_hr_when_present(self) -> None:
        h = self.harness

        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|FAIL|reason=wasapi_failed|hr=0x8007000e\n"
        msg = h._virtio_snd_buffer_limits_required_failure_message(tail)
        self.assertIsNotNone(msg)
        assert msg is not None
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_BUFFER_LIMITS_FAILED:"))
        self.assertIn("reason=wasapi_failed", msg)
        self.assertIn("hr=0x8007000e", msg)

    def test_virtio_blk_failed_token_includes_io_flags_and_irq_fields_when_present(self) -> None:
        h = self.harness

        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|FAIL|irq_mode=msix|irq_message_count=3|"
            b"write_ok=0|write_bytes=0|write_mbps=0.00|flush_ok=1|read_ok=0|read_bytes=0|read_mbps=0.00\n"
        )
        msg = h._virtio_blk_fail_failure_message(tail)
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_BLK_FAILED:"))
        self.assertIn("write_ok=0", msg)
        self.assertIn("flush_ok=1", msg)
        self.assertIn("read_ok=0", msg)
        self.assertIn("irq_mode=msix", msg)
        self.assertIn("irq_message_count=3", msg)

    def test_virtio_net_failed_token_includes_large_and_upload_fields_when_present(self) -> None:
        h = self.harness

        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|FAIL|large_ok=0|large_bytes=123|large_fnv1a64=0x0000000000000000|"
            b"large_mbps=0.00|upload_ok=1|upload_bytes=456|upload_mbps=1.23|msi=1|msi_messages=3|irq_mode=msi|irq_message_count=1\n"
        )
        msg = h._virtio_net_fail_failure_message(tail)
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_NET_FAILED:"))
        self.assertIn("large_ok=0", msg)
        self.assertIn("upload_ok=1", msg)
        self.assertIn("msi_messages=3", msg)
        self.assertIn("irq_mode=msi", msg)

    def test_virtio_input_failed_token_includes_reason_and_device_counts_when_present(self) -> None:
        h = self.harness

        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|FAIL|devices=1|keyboard_devices=0|consumer_devices=0|"
            b"mouse_devices=1|ambiguous_devices=0|unknown_devices=0|keyboard_collections=0|consumer_collections=0|"
            b"mouse_collections=1|tablet_devices=0|tablet_collections=0|reason=device_missing|irq_mode=intx|irq_message_count=0\n"
        )
        msg = h._virtio_input_fail_failure_message(tail)
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_INPUT_FAILED:"))
        self.assertIn("reason=device_missing", msg)
        self.assertIn("devices=1", msg)
        self.assertIn("mouse_devices=1", msg)
        self.assertIn("irq_mode=intx", msg)

    def test_virtio_input_bind_failed_token_includes_reason_and_expected_actual_when_present(self) -> None:
        h = self.harness

        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|FAIL|reason=wrong_service|expected=aero_virtio_input|"
            b"actual=other_service|pnp_id=PCI\\\\VEN_1AF4&DEV_1052&REV_01\\\\3&11583659&0&10|devices=1|"
            b"wrong_service=1|missing_service=0|problem=0\n"
        )
        msg = h._virtio_input_bind_fail_failure_message(tail)
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_INPUT_BIND_FAILED:"))
        self.assertIn("reason=wrong_service", msg)
        self.assertIn("expected=aero_virtio_input", msg)
        self.assertIn("actual=other_service", msg)
        self.assertIn("pnp_id=PCI\\\\VEN_1AF4&DEV_1052", msg)

    def test_virtio_input_events_extended_failed_token_includes_subtest_and_reason(self) -> None:
        h = self.harness

        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|FAIL|reason=missing_axis|err=5|"
            b"mouse_reports=10|mouse_bad_reports=0|wheel_total=0|hwheel_total=0|expected_wheel=1|"
            b"expected_hwheel=1|saw_wheel=0|saw_hwheel=0\n"
        )
        msg = h._virtio_input_events_extended_fail_failure_message(tail)
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_INPUT_EVENTS_EXTENDED_FAILED:"))
        self.assertIn("virtio-input-events-wheel", msg)
        self.assertIn("reason=missing_axis", msg)
        self.assertIn("err=5", msg)
        self.assertIn("wheel_total=0", msg)

    def test_virtio_snd_force_null_backend_token(self) -> None:
        h = self.harness

        tail = (
            b"virtio-snd: ForceNullBackend=1 set (pnp_id=PCI\\\\VEN_1AF4&DEV_1059&REV_01\\\\3&11583659&0&28 source=device_parameters); "
            b"virtio transport disabled (host wav capture will be silent)\n"
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|force_null_backend|irq_mode=msix|irq_message_count=3\n"
        )
        msg = h._try_virtio_snd_force_null_backend_failure_message(tail)
        self.assertIsNotNone(msg)
        assert msg is not None
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_FORCE_NULL_BACKEND:"))
        self.assertIn("ForceNullBackend", msg)
        self.assertIn(
            "HKLM\\SYSTEM\\CurrentControlSet\\Enum\\<DeviceInstancePath>\\Device Parameters\\Parameters\\ForceNullBackend",
            msg,
        )
        self.assertIn("pnp_id=", msg)
        self.assertIn("source=", msg)

        # Marker-only fallback (no diagnostic line).
        msg2 = h._try_virtio_snd_force_null_backend_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|force_null_backend\n"
        )
        self.assertIsNotNone(msg2)
        assert msg2 is not None
        self.assertRegex(msg2, _TOKEN_RE)
        self.assertTrue(msg2.startswith("FAIL: VIRTIO_SND_FORCE_NULL_BACKEND:"))

    def test_virtio_snd_capture_skip_tokens(self) -> None:
        h = self.harness

        msg = h._virtio_snd_capture_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|endpoint_missing\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_CAPTURE_SKIPPED:"))

        msg = h._virtio_snd_capture_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|flag_not_set\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_CAPTURE_SKIPPED:"))

        msg = h._virtio_snd_capture_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|wrong_service\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_CAPTURE_SKIPPED:"))

    def test_virtio_snd_duplex_skip_tokens(self) -> None:
        h = self.harness

        msg = h._virtio_snd_duplex_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|endpoint_missing\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_DUPLEX_SKIPPED:"))

        msg = h._virtio_snd_duplex_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|flag_not_set\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_DUPLEX_SKIPPED:"))

        msg = h._virtio_snd_duplex_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|device_missing\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_DUPLEX_SKIPPED:"))

    def test_virtio_snd_buffer_limits_skip_tokens(self) -> None:
        h = self.harness

        msg = h._virtio_snd_buffer_limits_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|SKIP|flag_not_set\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_BUFFER_LIMITS_SKIPPED:"))

    def test_virtio_input_tablet_events_skip_tokens(self) -> None:
        h = self.harness

        msg = h._virtio_input_tablet_events_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP|flag_not_set\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_INPUT_TABLET_EVENTS_SKIPPED:"))
        self.assertIn("flag_not_set", msg)

        msg = h._virtio_input_tablet_events_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP|no_tablet_device\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_INPUT_TABLET_EVENTS_SKIPPED:"))
        self.assertIn("no_tablet_device", msg)
        self.assertNotIn("flag_not_set", msg)

        msg = h._virtio_snd_buffer_limits_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|SKIP|disabled\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_BUFFER_LIMITS_SKIPPED:"))

    def test_virtio_blk_recovery_nonzero_token(self) -> None:
        h = self.harness

        msg = h._check_no_blk_recovery_requirement(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|abort_srb=0|reset_device_srb=1|reset_bus_srb=0|pnp_srb=0|ioctl_reset=0\n"
        )
        self.assertIsNotNone(msg)
        assert msg is not None
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_BLK_RECOVERY_NONZERO:"))

    def test_virtio_blk_counters_recovery_detected_token(self) -> None:
        h = self.harness

        msg = h._check_fail_on_blk_recovery_requirement(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|INFO|abort=0|reset_device=1|reset_bus=0|pnp=0|ioctl_reset=0|capacity_change_events=0\n"
        )
        self.assertIsNotNone(msg)
        assert msg is not None
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_BLK_RECOVERY_DETECTED:"))

    def test_virtio_blk_reset_recovery_nonzero_token(self) -> None:
        h = self.harness

        msg = h._check_no_blk_reset_recovery_requirement(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|INFO|reset_detected=1|hw_reset_bus=0\n"
        )
        self.assertIsNotNone(msg)
        assert msg is not None
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_BLK_RESET_RECOVERY_NONZERO:"))

    def test_virtio_blk_reset_recovery_detected_token(self) -> None:
        h = self.harness

        msg = h._check_fail_on_blk_reset_recovery_requirement(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|INFO|reset_detected=1|hw_reset_bus=2\n"
        )
        self.assertIsNotNone(msg)
        assert msg is not None
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_BLK_RESET_RECOVERY_DETECTED:"))

    def test_virtio_blk_miniport_flags_nonzero_token(self) -> None:
        h = self.harness

        msg = h._check_no_blk_miniport_flags_requirement(
            b"virtio-blk-miniport-flags|INFO|raw=0x00000001|removed=1|surprise_removed=0|reset_in_progress=0|reset_pending=0\n"
        )
        self.assertIsNotNone(msg)
        assert msg is not None
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_BLK_MINIPORT_FLAGS_NONZERO:"))

    def test_virtio_blk_miniport_flags_removed_token(self) -> None:
        h = self.harness

        msg = h._check_fail_on_blk_miniport_flags_requirement(
            b"virtio-blk-miniport-flags|INFO|raw=0x00000003|removed=1|surprise_removed=1|reset_in_progress=0|reset_pending=0\n"
        )
        self.assertIsNotNone(msg)
        assert msg is not None
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_BLK_MINIPORT_FLAGS_REMOVED:"))

    def test_virtio_blk_reset_skip_tokens(self) -> None:
        h = self.harness

        msg = h._virtio_blk_reset_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP|reason=not_supported\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_BLK_RESET_SKIPPED:"))

        # If the guest is new enough to emit a skip marker when the test is not enabled, ensure
        # the failure message includes a provisioning hint.
        msg = h._virtio_blk_reset_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP|reason=flag_not_set\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_BLK_RESET_SKIPPED:"))
        self.assertIn("--test-blk-reset", msg)

        # Backcompat: older selftests may emit `...|SKIP|flag_not_set` (no `reason=` key).
        msg = h._virtio_blk_reset_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP|flag_not_set\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_BLK_RESET_SKIPPED:"))
        self.assertIn("--test-blk-reset", msg)

    def test_virtio_blk_reset_fail_tokens_include_reason_and_err_when_present(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL|reason=post_reset_io_failed|err=123\n"
        msg = h._virtio_blk_reset_required_failure_message(tail)
        self.assertIsNotNone(msg)
        assert msg is not None
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_BLK_RESET_FAILED:"))
        self.assertIn("reason=post_reset_io_failed", msg)
        self.assertIn("err=123", msg)

        # Backcompat: older selftests may emit `...|FAIL|post_reset_io_failed|err=123` (no `reason=` field).
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL|post_reset_io_failed|err=123\n"
        msg = h._virtio_blk_reset_required_failure_message(tail)
        self.assertIsNotNone(msg)
        assert msg is not None
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_BLK_RESET_FAILED:"))
        self.assertIn("reason=post_reset_io_failed", msg)
        self.assertIn("err=123", msg)

    def test_virtio_blk_reset_missing_token(self) -> None:
        h = self.harness

        msg = h._virtio_blk_reset_missing_failure_message()
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: MISSING_VIRTIO_BLK_RESET:"))

    def test_virtio_blk_resize_skip_tokens_include_provisioning_hint(self) -> None:
        h = self.harness
        msg = h._virtio_blk_resize_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|SKIP|flag_not_set\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_BLK_RESIZE_SKIPPED:"))
        self.assertIn("--test-blk-resize", msg)

    def test_virtio_blk_resize_fail_tokens_include_reason_and_err(self) -> None:
        h = self.harness
        msg = h._virtio_blk_resize_fail_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|FAIL|reason=timeout|disk=1|old_bytes=512|last_bytes=512|err=1460\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_BLK_RESIZE_FAILED:"))
        self.assertIn("reason=timeout", msg)
        self.assertIn("err=1460", msg)
        self.assertIn("disk=1", msg)
        self.assertIn("old_bytes=512", msg)
        self.assertIn("last_bytes=512", msg)

    def test_virtio_net_link_flap_skip_tokens(self) -> None:
        h = self.harness

        msg = h._virtio_net_link_flap_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|SKIP|flag_not_set\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_NET_LINK_FLAP_SKIPPED:"))

    def test_virtio_net_udp_fail_tokens_include_reason_and_wsa(self) -> None:
        h = self.harness

        msg = h._virtio_net_udp_fail_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|FAIL|bytes=0|small_bytes=0|mtu_bytes=0|reason=timeout|wsa=10060\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_NET_UDP_FAILED:"))
        self.assertIn("reason=timeout", msg)
        self.assertIn("wsa=10060", msg)
        self.assertIn("bytes=0", msg)

    def test_virtio_net_udp_skipped_token_includes_reason_when_present(self) -> None:
        h = self.harness

        msg = h._virtio_net_udp_skip_failure_message(
            b"",
            marker_line="AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|SKIP|flag_not_set",
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_NET_UDP_SKIPPED:"))
        self.assertIn("flag_not_set", msg)

    def test_virtio_input_events_fail_tokens_include_reason_and_err(self) -> None:
        h = self.harness

        msg = h._virtio_input_events_fail_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|FAIL|reason=timeout|err=5|kbd_reports=1|mouse_reports=2|kbd_bad_reports=0|mouse_bad_reports=0\n",
            req_flags_desc="--with-input-events",
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_INPUT_EVENTS_FAILED:"))
        self.assertIn("reason=timeout", msg)
        self.assertIn("err=5", msg)
        self.assertIn("kbd_reports=1", msg)
        self.assertIn("--with-input-events", msg)

    def test_virtio_input_media_keys_fail_tokens_include_reason_and_err(self) -> None:
        h = self.harness

        msg = h._virtio_input_media_keys_fail_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|FAIL|reason=timeout|err=5|reports=1|volume_up_down=1|volume_up_up=1\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_INPUT_MEDIA_KEYS_FAILED:"))
        self.assertIn("reason=timeout", msg)
        self.assertIn("err=5", msg)
        self.assertIn("reports=1", msg)

    def test_virtio_input_led_fail_tokens_include_reason_and_err(self) -> None:
        h = self.harness
        msg = h._virtio_input_led_fail_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|FAIL|reason=timeout|err=1460|sent=0|format=out_report|led=num_lock\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_INPUT_LED_FAILED:"))
        self.assertIn("reason=timeout", msg)
        self.assertIn("err=1460", msg)
        self.assertIn("sent=0", msg)

    def test_virtio_input_leds_fail_tokens_include_reason_and_err(self) -> None:
        h = self.harness
        msg = h._virtio_input_leds_fail_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|FAIL|reason=timeout|err=1460|writes=1\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_INPUT_LEDS_FAILED:"))
        self.assertIn("reason=timeout", msg)
        self.assertIn("err=1460", msg)
        self.assertIn("writes=1", msg)

    def test_virtio_input_tablet_events_skip_tokens(self) -> None:
        h = self.harness

        msg = h._virtio_input_tablet_events_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP|flag_not_set\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_INPUT_TABLET_EVENTS_SKIPPED:"))
        self.assertIn("--test-input-tablet-events", msg)
        self.assertIn("--test-tablet-events", msg)

        # virtio-input-tablet-events can also be skipped for reasons other than provisioning.
        msg2 = h._virtio_input_tablet_events_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP|no_tablet_device\n"
        )
        self.assertRegex(msg2, _TOKEN_RE)
        self.assertTrue(msg2.startswith("FAIL: VIRTIO_INPUT_TABLET_EVENTS_SKIPPED:"))
        self.assertIn("no_tablet_device", msg2)
        self.assertNotIn("--test-input-tablet-events", msg2)

    def test_virtio_input_tablet_events_fail_tokens_include_reason_and_err(self) -> None:
        h = self.harness

        msg = h._virtio_input_tablet_events_fail_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|FAIL|reason=timeout|err=123|tablet_reports=0|move_target=0|left_down=0|left_up=0|last_x=0|last_y=0|last_left=0\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_INPUT_TABLET_EVENTS_FAILED:"))
        self.assertIn("reason=timeout", msg)
        self.assertIn("err=123", msg)
        self.assertIn("tablet_reports=0", msg)

    def test_virtio_input_wheel_skip_tokens_include_reason_details(self) -> None:
        h = self.harness

        msg = h._virtio_input_wheel_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|SKIP|flag_not_set\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_INPUT_WHEEL_SKIPPED:"))
        self.assertIn("flag_not_set", msg)
        self.assertIn("--test-input-events", msg)

        msg2 = h._virtio_input_wheel_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|SKIP|not_observed|wheel_total=0|hwheel_total=0\n"
        )
        self.assertRegex(msg2, _TOKEN_RE)
        self.assertTrue(msg2.startswith("FAIL: VIRTIO_INPUT_WHEEL_SKIPPED:"))
        self.assertIn("not_observed", msg2)
        self.assertIn("wheel_total=0", msg2)
        self.assertNotIn("--test-input-events", msg2)

        msg3 = h._virtio_input_wheel_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|SKIP|input_events_failed|reason=timeout|err=5|wheel_total=0|hwheel_total=0\n"
        )
        self.assertRegex(msg3, _TOKEN_RE)
        self.assertTrue(msg3.startswith("FAIL: VIRTIO_INPUT_WHEEL_SKIPPED:"))
        self.assertIn("input_events_failed", msg3)
        self.assertIn("reason=timeout", msg3)
        self.assertIn("err=5", msg3)

    def test_virtio_input_wheel_fail_tokens_include_reason_and_counters(self) -> None:
        h = self.harness
        msg = h._virtio_input_wheel_fail_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|FAIL|reason=missing_axis|wheel_total=0|hwheel_total=120|saw_wheel=0|saw_hwheel=1\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_INPUT_WHEEL_FAILED:"))
        self.assertIn("reason=missing_axis", msg)
        self.assertIn("wheel_total=0", msg)
        self.assertIn("hwheel_total=120", msg)
        self.assertIn("saw_hwheel=1", msg)

    def test_virtio_input_leds_required_tokens(self) -> None:
        h = self.harness

        msg = h._virtio_input_leds_required_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|SKIP|flag_not_set\n"
        )
        assert msg is not None
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_INPUT_LEDS_SKIPPED:"))

        msg = h._virtio_input_leds_required_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|FAIL|reason=timeout\n"
        )
        assert msg is not None
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_INPUT_LEDS_FAILED:"))

        msg = h._virtio_input_leds_required_failure_message(b"unrelated log output\n")
        assert msg is not None
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: MISSING_VIRTIO_INPUT_LEDS:"))

        self.assertIsNone(
            h._virtio_input_leds_required_failure_message(
                b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|PASS|writes=3\n"
            )
        )


if __name__ == "__main__":
    unittest.main()
