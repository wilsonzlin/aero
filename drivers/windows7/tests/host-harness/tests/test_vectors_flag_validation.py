#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import contextlib
import importlib.util
import io
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


class HarnessVectorsFlagValidationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_virtio_snd_vectors_requires_with_virtio_snd(self) -> None:
        h = self.harness

        old_argv = sys.argv
        try:
            sys.argv = [
                "invoke_aero_virtio_win7_tests.py",
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                "disk.img",
                "--virtio-snd-vectors",
                "2",
            ]
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as cm:
                h.main()
            # argparse uses exit code 2 for CLI usage errors.
            self.assertEqual(cm.exception.code, 2)
            self.assertIn("--virtio-snd-vectors requires --with-virtio-snd", stderr.getvalue())
        finally:
            sys.argv = old_argv

    def test_virtio_snd_msix_vectors_alias_requires_with_virtio_snd(self) -> None:
        h = self.harness

        old_argv = sys.argv
        try:
            sys.argv = [
                "invoke_aero_virtio_win7_tests.py",
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                "disk.img",
                "--virtio-snd-msix-vectors",
                "2",
            ]
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as cm:
                h.main()
            # argparse uses exit code 2 for CLI usage errors.
            self.assertEqual(cm.exception.code, 2)
            self.assertIn("--virtio-snd-vectors requires --with-virtio-snd", stderr.getvalue())
        finally:
            sys.argv = old_argv

    def test_snd_buffer_limits_requires_with_virtio_snd(self) -> None:
        h = self.harness

        old_argv = sys.argv
        try:
            sys.argv = [
                "invoke_aero_virtio_win7_tests.py",
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                "disk.img",
                "--with-snd-buffer-limits",
            ]
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as cm:
                h.main()
            self.assertEqual(cm.exception.code, 2)
            self.assertIn("--with-snd-buffer-limits requires --with-virtio-snd", stderr.getvalue())
        finally:
            sys.argv = old_argv

    def test_rejects_non_positive_vectors_flags(self) -> None:
        h = self.harness

        cases = [
            (["--virtio-msix-vectors", "0"], "--virtio-msix-vectors must be a positive integer"),
            (["--virtio-msix-vectors", "-1"], "--virtio-msix-vectors must be a positive integer"),
            (["--virtio-net-vectors", "0"], "--virtio-net-vectors must be a positive integer"),
            (["--virtio-net-msix-vectors", "0"], "--virtio-net-vectors must be a positive integer"),
            (["--virtio-blk-vectors", "0"], "--virtio-blk-vectors must be a positive integer"),
            (["--virtio-blk-msix-vectors", "0"], "--virtio-blk-vectors must be a positive integer"),
            (["--virtio-input-vectors", "0"], "--virtio-input-vectors must be a positive integer"),
            (["--virtio-input-msix-vectors", "0"], "--virtio-input-vectors must be a positive integer"),
            (["--virtio-snd-vectors", "0"], "--virtio-snd-vectors must be a positive integer"),
            (["--virtio-snd-msix-vectors", "0"], "--virtio-snd-vectors must be a positive integer"),
        ]

        for extra_argv, expected in cases:
            with self.subTest(argv=extra_argv):
                old_argv = sys.argv
                try:
                    sys.argv = [
                        "invoke_aero_virtio_win7_tests.py",
                        "--qemu-system",
                        "qemu-system-x86_64",
                        "--disk-image",
                        "disk.img",
                    ] + extra_argv
                    stderr = io.StringIO()
                    with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as cm:
                        h.main()
                    self.assertEqual(cm.exception.code, 2)
                    self.assertIn(expected, stderr.getvalue())
                finally:
                    sys.argv = old_argv

    def test_virtio_disable_msix_mutually_exclusive_with_vectors(self) -> None:
        h = self.harness

        cases = [
            (
                ["--virtio-disable-msix", "--virtio-msix-vectors", "2"],
                "--virtio-disable-msix is mutually exclusive",
            ),
            (
                ["--virtio-disable-msix", "--virtio-net-vectors", "2"],
                "--virtio-disable-msix is mutually exclusive",
            ),
            (
                ["--virtio-disable-msix", "--virtio-input-vectors", "2"],
                "--virtio-disable-msix is mutually exclusive",
            ),
        ]

        for extra_argv, expected in cases:
            with self.subTest(argv=extra_argv):
                old_argv = sys.argv
                try:
                    sys.argv = [
                        "invoke_aero_virtio_win7_tests.py",
                        "--qemu-system",
                        "qemu-system-x86_64",
                        "--disk-image",
                        "disk.img",
                    ] + extra_argv
                    stderr = io.StringIO()
                    with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as cm:
                        h.main()
                    self.assertEqual(cm.exception.code, 2)
                    self.assertIn(expected, stderr.getvalue())
                finally:
                    sys.argv = old_argv

    def test_virtio_disable_msix_incompatible_with_require_msix(self) -> None:
        h = self.harness

        old_argv = sys.argv
        try:
            sys.argv = [
                "invoke_aero_virtio_win7_tests.py",
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                "disk.img",
                "--virtio-disable-msix",
                "--require-virtio-net-msix",
            ]
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as cm:
                h.main()
            self.assertEqual(cm.exception.code, 2)
            self.assertIn("--virtio-disable-msix is incompatible with --require-virtio-*-msix", stderr.getvalue())
        finally:
            sys.argv = old_argv

    def test_require_intx_and_require_msi_mutually_exclusive(self) -> None:
        h = self.harness

        old_argv = sys.argv
        try:
            sys.argv = [
                "invoke_aero_virtio_win7_tests.py",
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                "disk.img",
                "--require-intx",
                "--require-msi",
            ]
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as cm:
                h.main()
            self.assertEqual(cm.exception.code, 2)
            self.assertIn("--require-intx and --require-msi are mutually exclusive", stderr.getvalue())
        finally:
            sys.argv = old_argv

    def test_rejects_udp_port_out_of_range(self) -> None:
        h = self.harness

        cases = [
            (["--udp-port", "0"], "--udp-port must be in the range 1..65535"),
            (["--udp-port", "-1"], "--udp-port must be in the range 1..65535"),
            (["--udp-port", "65536"], "--udp-port must be in the range 1..65535"),
        ]

        for extra_argv, expected in cases:
            with self.subTest(argv=extra_argv):
                old_argv = sys.argv
                try:
                    sys.argv = [
                        "invoke_aero_virtio_win7_tests.py",
                        "--qemu-system",
                        "qemu-system-x86_64",
                        "--disk-image",
                        "disk.img",
                    ] + extra_argv
                    stderr = io.StringIO()
                    with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as cm:
                        h.main()
                    self.assertEqual(cm.exception.code, 2)
                    self.assertIn(expected, stderr.getvalue())
                finally:
                    sys.argv = old_argv

    def test_rejects_http_port_out_of_range(self) -> None:
        h = self.harness

        cases = [
            (["--http-port", "0"], "--http-port must be in the range 1..65535"),
            (["--http-port", "-1"], "--http-port must be in the range 1..65535"),
            (["--http-port", "65536"], "--http-port must be in the range 1..65535"),
        ]

        for extra_argv, expected in cases:
            with self.subTest(argv=extra_argv):
                old_argv = sys.argv
                try:
                    sys.argv = [
                        "invoke_aero_virtio_win7_tests.py",
                        "--qemu-system",
                        "qemu-system-x86_64",
                        "--disk-image",
                        "disk.img",
                    ] + extra_argv
                    stderr = io.StringIO()
                    with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as cm:
                        h.main()
                    self.assertEqual(cm.exception.code, 2)
                    self.assertIn(expected, stderr.getvalue())
                finally:
                    sys.argv = old_argv

    def test_rejects_non_positive_memory_smp_timeout(self) -> None:
        h = self.harness

        cases = [
            (["--memory-mb", "0"], "--memory-mb must be a positive integer"),
            (["--memory-mb", "-1"], "--memory-mb must be a positive integer"),
            (["--smp", "0"], "--smp must be a positive integer"),
            (["--smp", "-1"], "--smp must be a positive integer"),
            (["--timeout-seconds", "0"], "--timeout-seconds must be a positive integer"),
            (["--timeout-seconds", "-1"], "--timeout-seconds must be a positive integer"),
        ]

        for extra_argv, expected in cases:
            with self.subTest(argv=extra_argv):
                old_argv = sys.argv
                try:
                    sys.argv = [
                        "invoke_aero_virtio_win7_tests.py",
                        "--qemu-system",
                        "qemu-system-x86_64",
                        "--disk-image",
                        "disk.img",
                    ] + extra_argv
                    stderr = io.StringIO()
                    with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as cm:
                        h.main()
                    self.assertEqual(cm.exception.code, 2)
                    self.assertIn(expected, stderr.getvalue())
                finally:
                    sys.argv = old_argv

    def test_rejects_http_path_without_leading_slash(self) -> None:
        h = self.harness

        old_argv = sys.argv
        try:
            sys.argv = [
                "invoke_aero_virtio_win7_tests.py",
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                "disk.img",
                "--http-path",
                "aero-virtio-selftest",
            ]
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as cm:
                h.main()
            self.assertEqual(cm.exception.code, 2)
            self.assertIn("--http-path must start with '/'", stderr.getvalue())
        finally:
            sys.argv = old_argv

    def test_rejects_negative_wav_thresholds_when_verify_enabled(self) -> None:
        h = self.harness

        cases = [
            (
                ["--virtio-snd-wav-peak-threshold", "-1"],
                "--virtio-snd-wav-peak-threshold must be >= 0",
            ),
            (
                ["--virtio-snd-wav-rms-threshold", "-1"],
                "--virtio-snd-wav-rms-threshold must be >= 0",
            ),
        ]

        for extra_argv, expected in cases:
            with self.subTest(argv=extra_argv):
                old_argv = sys.argv
                try:
                    sys.argv = [
                        "invoke_aero_virtio_win7_tests.py",
                        "--qemu-system",
                        "qemu-system-x86_64",
                        "--disk-image",
                        "disk.img",
                        "--with-virtio-snd",
                        "--virtio-snd-audio-backend",
                        "wav",
                        "--virtio-snd-wav-path",
                        "out.wav",
                        "--virtio-snd-verify-wav",
                    ] + extra_argv
                    stderr = io.StringIO()
                    with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as cm:
                        h.main()
                    self.assertEqual(cm.exception.code, 2)
                    self.assertIn(expected, stderr.getvalue())
                finally:
                    sys.argv = old_argv

    def test_rejects_disk_image_directory(self) -> None:
        h = self.harness

        import tempfile
        from pathlib import Path

        with tempfile.TemporaryDirectory() as td:
            disk_dir = Path(td)
            old_argv = sys.argv
            try:
                sys.argv = [
                    "invoke_aero_virtio_win7_tests.py",
                    "--qemu-system",
                    "qemu-system-x86_64",
                    "--disk-image",
                    str(disk_dir),
                    "--dry-run",
                ]
                stderr = io.StringIO()
                with contextlib.redirect_stderr(stderr):
                    rc = h.main()
                self.assertEqual(rc, 2)
                self.assertIn("ERROR: disk image path is a directory", stderr.getvalue())
            finally:
                sys.argv = old_argv

    def test_rejects_serial_log_directory(self) -> None:
        h = self.harness

        import tempfile
        from pathlib import Path

        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            serial_dir = tmp / "serial_dir"
            serial_dir.mkdir()
            disk = tmp / "disk.img"
            # Keep disk missing; dry-run mode should still reach serial-log validation.

            old_argv = sys.argv
            try:
                sys.argv = [
                    "invoke_aero_virtio_win7_tests.py",
                    "--qemu-system",
                    "qemu-system-x86_64",
                    "--disk-image",
                    str(disk),
                    "--serial-log",
                    str(serial_dir),
                    "--dry-run",
                ]
                stderr = io.StringIO()
                with contextlib.redirect_stderr(stderr):
                    rc = h.main()
                self.assertEqual(rc, 2)
                self.assertIn("ERROR: serial log path is a directory", stderr.getvalue())
            finally:
                sys.argv = old_argv

    def test_rejects_qemu_stderr_log_directory(self) -> None:
        h = self.harness

        import tempfile
        from pathlib import Path

        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            disk = tmp / "disk.img"
            disk.write_bytes(b"")
            serial = tmp / "serial.log"
            qemu_stderr = tmp / "serial.qemu.stderr.log"
            qemu_stderr.mkdir()

            old_argv = sys.argv
            try:
                sys.argv = [
                    "invoke_aero_virtio_win7_tests.py",
                    "--qemu-system",
                    "qemu-system-x86_64",
                    "--virtio-transitional",
                    "--disk-image",
                    str(disk),
                    "--serial-log",
                    str(serial),
                ]
                stderr = io.StringIO()
                with contextlib.redirect_stderr(stderr):
                    rc = h.main()
                self.assertEqual(rc, 2)
                self.assertIn("ERROR: qemu stderr log path is a directory", stderr.getvalue())
            finally:
                sys.argv = old_argv


if __name__ == "__main__":
    unittest.main()
