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
