#!/usr/bin/env python3

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


class SetupInfAddServiceTests(unittest.TestCase):
    def _run_selftest(self, setup_cmd: Path, inf: Path, service: str) -> subprocess.CompletedProcess[str]:
        # Use `call` so cmd.exe properly forwards the called batch file's exit code.
        cmd_line = f'call "{setup_cmd}" /_selftest_inf_addservice "{inf}" "{service}"'
        comspec = os.environ.get("ComSpec", "cmd.exe")
        return subprocess.run(
            [comspec, "/d", "/c", cmd_line],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )

    def _run_validate_selftest(
        self, setup_cmd: Path, driver_dir: Path, service: str
    ) -> subprocess.CompletedProcess[str]:
        cmd_line = f'call "{setup_cmd}" /_selftest_validate_storage_service_infs "{driver_dir}" "{service}"'
        comspec = os.environ.get("ComSpec", "cmd.exe")
        return subprocess.run(
            [comspec, "/d", "/c", cmd_line],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )

    @unittest.skipUnless(os.name == "nt", "requires Windows cmd.exe")
    def test_utf16_inf_addservice_detection(self) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        setup_cmd = repo_root / "guest-tools" / "setup.cmd"
        self.assertTrue(setup_cmd.exists(), f"missing setup.cmd at: {setup_cmd}")

        service = "viostor"

        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-inf-addservice-") as tmp:
            tmp_path = Path(tmp)

            ok_inf = tmp_path / "ok.inf"
            ok_inf.write_bytes(
                (
                    "\r\n".join(
                        [
                            "; UTF-16LE INF fixture",
                            "[Version]",
                            'Signature="$Windows NT$"',
                            "[DefaultInstall.NT]",
                            f'AddService = "{service}", 0x00000002, Service_Inst',
                        ]
                    )
                    + "\r\n"
                ).encode("utf-16")
            )

            commented_inf = tmp_path / "commented.inf"
            commented_inf.write_bytes(
                (
                    "\r\n".join(
                        [
                            "; UTF-16LE INF fixture",
                            "[DefaultInstall.NT]",
                            f'   ; AddService = "{service}", 0x00000002, Service_Inst',
                            "; No real AddService assignment in this file.",
                        ]
                    )
                    + "\r\n"
                ).encode("utf-16")
            )

            ansi_ok_inf = tmp_path / "ok-ansi.inf"
            ansi_ok_inf.write_text(
                "\r\n".join(
                    [
                        "; UTF-8/ANSI INF fixture",
                        "[Version]",
                        'Signature="$Windows NT$"',
                        "[DefaultInstall.NT]",
                        f"AddService = {service}, 0x00000002, Service_Inst",
                    ]
                )
                + "\r\n",
                encoding="utf-8",
            )

            ansi_ok_tabs_inf = tmp_path / "ok-ansi-tabs.inf"
            ansi_ok_tabs_inf.write_text(
                "\r\n".join(
                    [
                        "; UTF-8/ANSI INF fixture (tabs around tokens)",
                        "[Version]",
                        'Signature="$Windows NT$"',
                        "[DefaultInstall.NT]",
                        f"AddService\t=\t{service}\t, 0x00000002, Service_Inst",
                    ]
                )
                + "\r\n",
                encoding="utf-8",
            )

            ansi_ok_inline_comment_inf = tmp_path / "ok-ansi-inline-comment.inf"
            ansi_ok_inline_comment_inf.write_text(
                "\r\n".join(
                    [
                        "; UTF-8/ANSI INF fixture (inline comment before comma)",
                        "[DefaultInstall.NT]",
                        f"AddService = {service}; some comment, 0x00000002, Service_Inst",
                    ]
                )
                + "\r\n",
                encoding="utf-8",
            )

            ansi_commented_inf = tmp_path / "commented-ansi.inf"
            ansi_commented_inf.write_text(
                "\r\n".join(
                    [
                        "; UTF-8/ANSI INF fixture",
                        "[DefaultInstall.NT]",
                        f"   ; AddService = {service}, 0x00000002, Service_Inst",
                        "; No real AddService assignment in this file.",
                    ]
                )
                + "\r\n",
                encoding="utf-8",
            )

            ok = self._run_selftest(setup_cmd, ok_inf, service)
            self.assertEqual(
                ok.returncode,
                0,
                msg=f"expected ok.inf to match AddService={service} (exit 0), got {ok.returncode}. Output:\n{ok.stdout}",
            )

            commented = self._run_selftest(setup_cmd, commented_inf, service)
            self.assertEqual(
                commented.returncode,
                1,
                msg=(
                    f"expected commented.inf to NOT match AddService={service} (exit 1), got {commented.returncode}. "
                    f"Output:\n{commented.stdout}"
                ),
            )

            ansi_ok = self._run_selftest(setup_cmd, ansi_ok_inf, service)
            self.assertEqual(
                ansi_ok.returncode,
                0,
                msg=f"expected ok-ansi.inf to match AddService={service} (exit 0), got {ansi_ok.returncode}. Output:\n{ansi_ok.stdout}",
            )

            ansi_ok_tabs = self._run_selftest(setup_cmd, ansi_ok_tabs_inf, service)
            self.assertEqual(
                ansi_ok_tabs.returncode,
                0,
                msg=f"expected ok-ansi-tabs.inf to match AddService={service} (exit 0), got {ansi_ok_tabs.returncode}. Output:\n{ansi_ok_tabs.stdout}",
            )

            ansi_ok_inline_comment = self._run_selftest(setup_cmd, ansi_ok_inline_comment_inf, service)
            self.assertEqual(
                ansi_ok_inline_comment.returncode,
                0,
                msg=(
                    f"expected ok-ansi-inline-comment.inf to match AddService={service} (exit 0), got {ansi_ok_inline_comment.returncode}. "
                    f"Output:\n{ansi_ok_inline_comment.stdout}"
                ),
            )

            ansi_commented = self._run_selftest(setup_cmd, ansi_commented_inf, service)
            self.assertEqual(
                ansi_commented.returncode,
                1,
                msg=(
                    f"expected commented-ansi.inf to NOT match AddService={service} (exit 1), got {ansi_commented.returncode}. "
                    f"Output:\n{ansi_commented.stdout}"
                ),
            )

    @unittest.skipUnless(os.name == "nt", "requires Windows cmd.exe")
    def test_validate_storage_service_infs_finds_utf16_driver(self) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        setup_cmd = repo_root / "guest-tools" / "setup.cmd"
        self.assertTrue(setup_cmd.exists(), f"missing setup.cmd at: {setup_cmd}")

        service = "viostor"

        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-inf-validate-") as tmp:
            tmp_path = Path(tmp)

            # One UTF-16 INF that defines the service.
            ok_inf = tmp_path / "ok.inf"
            ok_inf.write_bytes(
                (
                    "\r\n".join(
                        [
                            "; UTF-16LE INF fixture",
                            "[Version]",
                            'Signature="$Windows NT$"',
                            "[DefaultInstall.NT]",
                            f'AddService = "{service}", 0x00000002, Service_Inst',
                        ]
                    )
                    + "\r\n"
                ).encode("utf-16")
            )

            # Many ASCII INFs that do not contain AddService at all should not cause a false
            # negative (and should not require per-INF PowerShell fallback).
            for i in range(10):
                (tmp_path / f"noop-{i}.inf").write_text(
                    "\r\n".join(
                        [
                            "; ASCII INF fixture",
                            "[Version]",
                            'Signature="$Windows NT$"',
                        ]
                    )
                    + "\r\n",
                    encoding="utf-8",
                )

            result = self._run_validate_selftest(setup_cmd, tmp_path, service)
            self.assertEqual(
                result.returncode,
                0,
                msg=f"expected validation to succeed (exit 0), got {result.returncode}. Output:\n{result.stdout}",
            )

    @unittest.skipUnless(os.name == "nt", "requires Windows cmd.exe")
    def test_validate_storage_service_infs_finds_utf16_driver_unquoted(self) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        setup_cmd = repo_root / "guest-tools" / "setup.cmd"
        self.assertTrue(setup_cmd.exists(), f"missing setup.cmd at: {setup_cmd}")

        service = "viostor"

        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-inf-validate-") as tmp:
            tmp_path = Path(tmp)

            ok_inf = tmp_path / "ok-unquoted.inf"
            ok_inf.write_bytes(
                (
                    "\r\n".join(
                        [
                            "; UTF-16LE INF fixture",
                            "[Version]",
                            'Signature="$Windows NT$"',
                            "[DefaultInstall.NT]",
                            f"AddService = {service}, 0x00000002, Service_Inst",
                        ]
                    )
                    + "\r\n"
                ).encode("utf-16")
            )

            result = self._run_validate_selftest(setup_cmd, tmp_path, service)
            self.assertEqual(
                result.returncode,
                0,
                msg=f"expected validation to succeed for unquoted UTF-16 AddService (exit 0), got {result.returncode}. Output:\n{result.stdout}",
            )

    @unittest.skipUnless(os.name == "nt", "requires Windows cmd.exe")
    def test_validate_storage_service_infs_is_case_insensitive(self) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        setup_cmd = repo_root / "guest-tools" / "setup.cmd"
        self.assertTrue(setup_cmd.exists(), f"missing setup.cmd at: {setup_cmd}")

        target_service = "viostor"

        # UTF-16 path: ensure the PowerShell scan-list fallback matches OrdinalIgnoreCase.
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-inf-validate-") as tmp:
            tmp_path = Path(tmp)

            utf16_inf = tmp_path / "utf16.inf"
            utf16_inf.write_bytes(
                (
                    "\r\n".join(
                        [
                            "; UTF-16LE INF fixture",
                            "[DefaultInstall.NT]",
                            'AddService = "VioStor", 0x00000002, Service_Inst',
                        ]
                    )
                    + "\r\n"
                ).encode("utf-16")
            )

            result = self._run_validate_selftest(setup_cmd, tmp_path, target_service)
            self.assertEqual(
                result.returncode,
                0,
                msg=f"expected UTF-16 case-insensitive validation to succeed, got {result.returncode}. Output:\n{result.stdout}",
            )

        # ANSI path: ensure the findstr parser matches case-insensitively.
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-inf-validate-") as tmp:
            tmp_path = Path(tmp)

            ansi_inf = tmp_path / "ansi.inf"
            ansi_inf.write_text(
                "\r\n".join(
                    [
                        "; ANSI/UTF-8 INF fixture",
                        "[DefaultInstall.NT]",
                        "AddService = VIOSTOR, 0x00000002, Service_Inst",
                    ]
                )
                + "\r\n",
                encoding="utf-8",
            )

            result = self._run_validate_selftest(setup_cmd, tmp_path, target_service)
            self.assertEqual(
                result.returncode,
                0,
                msg=f"expected ANSI case-insensitive validation to succeed, got {result.returncode}. Output:\n{result.stdout}",
            )

    @unittest.skipUnless(os.name == "nt", "requires Windows cmd.exe")
    def test_validate_storage_service_infs_handles_tabs_in_ansi_inf(self) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        setup_cmd = repo_root / "guest-tools" / "setup.cmd"
        self.assertTrue(setup_cmd.exists(), f"missing setup.cmd at: {setup_cmd}")

        service = "viostor"

        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-inf-validate-") as tmp:
            tmp_path = Path(tmp)

            ansi_inf = tmp_path / "ansi-tabs.inf"
            ansi_inf.write_text(
                "\r\n".join(
                    [
                        "; ANSI/UTF-8 INF fixture (tabs)",
                        "[DefaultInstall.NT]",
                        f"AddService\t=\t{service}\t, 0x00000002, Service_Inst",
                    ]
                )
                + "\r\n",
                encoding="utf-8",
            )

            result = self._run_validate_selftest(setup_cmd, tmp_path, service)
            self.assertEqual(
                result.returncode,
                0,
                msg=f"expected validation to succeed for ANSI INF with tabs, got {result.returncode}. Output:\n{result.stdout}",
            )

    @unittest.skipUnless(os.name == "nt", "requires Windows cmd.exe")
    def test_validate_storage_service_infs_handles_inline_comment_before_comma(self) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        setup_cmd = repo_root / "guest-tools" / "setup.cmd"
        self.assertTrue(setup_cmd.exists(), f"missing setup.cmd at: {setup_cmd}")

        service = "viostor"

        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-inf-validate-") as tmp:
            tmp_path = Path(tmp)

            ansi_inf = tmp_path / "ansi-inline-comment.inf"
            ansi_inf.write_text(
                "\r\n".join(
                    [
                        "; ANSI/UTF-8 INF fixture (inline comment before comma)",
                        "[DefaultInstall.NT]",
                        f"AddService = {service}; comment, 0x00000002, Service_Inst",
                    ]
                )
                + "\r\n",
                encoding="utf-8",
            )

            result = self._run_validate_selftest(setup_cmd, tmp_path, service)
            self.assertEqual(
                result.returncode,
                0,
                msg=f"expected validation to succeed for inline-comment AddService, got {result.returncode}. Output:\n{result.stdout}",
            )

    @unittest.skipUnless(os.name == "nt", "requires Windows cmd.exe")
    def test_validate_storage_service_infs_handles_paths_with_spaces(self) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        setup_cmd = repo_root / "guest-tools" / "setup.cmd"
        self.assertTrue(setup_cmd.exists(), f"missing setup.cmd at: {setup_cmd}")

        service = "viostor"

        with tempfile.TemporaryDirectory(prefix="aero guest tools inf validate ") as tmp:
            tmp_path = Path(tmp)
            driver_dir = tmp_path / "drivers with spaces"
            driver_dir.mkdir(parents=True, exist_ok=True)

            # UTF-16 INF so the scan-list PowerShell fallback is exercised while the INF path
            # contains spaces.
            ok_inf = driver_dir / "ok.inf"
            ok_inf.write_bytes(
                (
                    "\r\n".join(
                        [
                            "; UTF-16LE INF fixture",
                            "[DefaultInstall.NT]",
                            f'AddService = "{service}", 0x00000002, Service_Inst',
                        ]
                    )
                    + "\r\n"
                ).encode("utf-16")
            )

            result = self._run_validate_selftest(setup_cmd, driver_dir, service)
            self.assertEqual(
                result.returncode,
                0,
                msg=f"expected validation to succeed with spacey paths, got {result.returncode}. Output:\n{result.stdout}",
            )

    @unittest.skipUnless(os.name == "nt", "requires Windows cmd.exe")
    def test_validate_storage_service_infs_handles_special_chars_in_paths(self) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        setup_cmd = repo_root / "guest-tools" / "setup.cmd"
        self.assertTrue(setup_cmd.exists(), f"missing setup.cmd at: {setup_cmd}")

        service = "viostor"

        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-inf-validate-") as tmp:
            tmp_path = Path(tmp)
            driver_dir = tmp_path / "drivers (a&b)"
            driver_dir.mkdir(parents=True, exist_ok=True)

            # UTF-16 INF so the scan-list PowerShell fallback is exercised, with special
            # characters in the path (parentheses and &).
            ok_inf = driver_dir / "ok.inf"
            ok_inf.write_bytes(
                (
                    "\r\n".join(
                        [
                            "; UTF-16LE INF fixture",
                            "[DefaultInstall.NT]",
                            f'AddService = "{service}", 0x00000002, Service_Inst',
                        ]
                    )
                    + "\r\n"
                ).encode("utf-16")
            )

            result = self._run_validate_selftest(setup_cmd, driver_dir, service)
            self.assertEqual(
                result.returncode,
                0,
                msg=f"expected validation to succeed with special chars in paths, got {result.returncode}. Output:\n{result.stdout}",
            )

    @unittest.skipUnless(os.name == "nt", "requires Windows cmd.exe")
    def test_validate_storage_service_infs_ignores_commented_utf16_addservice(self) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        setup_cmd = repo_root / "guest-tools" / "setup.cmd"
        self.assertTrue(setup_cmd.exists(), f"missing setup.cmd at: {setup_cmd}")

        service = "viostor"

        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-inf-validate-") as tmp:
            tmp_path = Path(tmp)

            # UTF-16 INF containing only a commented AddService mention.
            commented_inf = tmp_path / "commented.inf"
            commented_inf.write_bytes(
                (
                    "\r\n".join(
                        [
                            "; UTF-16LE INF fixture",
                            "[DefaultInstall.NT]",
                            f'   ; AddService = "{service}", 0x00000002, Service_Inst',
                        ]
                    )
                    + "\r\n"
                ).encode("utf-16")
            )

            # Extra noise INFs.
            for i in range(5):
                (tmp_path / f"noop-{i}.inf").write_text(
                    "\r\n".join(
                        [
                            "; ASCII INF fixture",
                            "[Version]",
                            'Signature="$Windows NT$"',
                        ]
                    )
                    + "\r\n",
                    encoding="utf-8",
                )

            result = self._run_validate_selftest(setup_cmd, tmp_path, service)
            self.assertEqual(
                result.returncode,
                13,
                msg=f"expected validation to fail with EC_STORAGE_SERVICE_MISMATCH=13, got {result.returncode}. Output:\n{result.stdout}",
            )

    @unittest.skipUnless(os.name == "nt", "requires Windows cmd.exe")
    def test_validate_storage_service_infs_ignores_commented_ansi_addservice(self) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        setup_cmd = repo_root / "guest-tools" / "setup.cmd"
        self.assertTrue(setup_cmd.exists(), f"missing setup.cmd at: {setup_cmd}")

        service = "viostor"

        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-inf-validate-") as tmp:
            tmp_path = Path(tmp)

            commented_inf = tmp_path / "commented-ansi.inf"
            commented_inf.write_text(
                "\r\n".join(
                    [
                        "; ANSI/UTF-8 INF fixture",
                        "[DefaultInstall.NT]",
                        f"  ; AddService = {service}, 0x00000002, Service_Inst",
                    ]
                )
                + "\r\n",
                encoding="utf-8",
            )

            result = self._run_validate_selftest(setup_cmd, tmp_path, service)
            self.assertEqual(
                result.returncode,
                13,
                msg=f"expected validation to fail with EC_STORAGE_SERVICE_MISMATCH=13, got {result.returncode}. Output:\n{result.stdout}",
            )


if __name__ == "__main__":
    unittest.main()
