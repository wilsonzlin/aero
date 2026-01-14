#!/usr/bin/env python3

import importlib.util
import io
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path


def _load_linter_module():
    repo_root = Path(__file__).resolve().parents[3]
    linter_path = repo_root / "tools/guest-tools/lint_guest_tools_scripts.py"
    spec = importlib.util.spec_from_file_location("lint_guest_tools_scripts", linter_path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Failed to import linter module from: {linter_path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


lint_guest_tools_scripts = _load_linter_module()

def _synthetic_setup_text(
    *,
    include_cdd_base_path: bool = True,
    include_check_mode: bool = True,
    include_skipstorage_flag: bool = True,
    include_storage_skip_marker: bool = True,
    include_cert_policy_gating: bool = True,
    include_cert_install_skip_policy: bool = True,
) -> str:
    lines: list[str] = []
    if include_cdd_base_path:
        lines.append(r"HKLM\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase")

    lines.extend(
        [
            "rem CriticalDeviceDatabase reference (not necessarily full path)",
            "CriticalDeviceDatabase",
            "AERO_VIRTIO_BLK_SERVICE",
            "AERO_VIRTIO_BLK_HWIDS",
            'reg.exe add "%SVC_KEY%" /v Start /t REG_DWORD /d 0 /f',
            'if /i "%SIGNING_POLICY%"=="test" echo ok',
            'if /i "%SIGNING_POLICY%"=="production" echo ok',
            'if /i "%SIGNING_POLICY%"=="none" echo ok',
            "/testsigning",
            "/nointegritychecks",
            "/forcesigningpolicy:none /forcesigningpolicy:test /forcesigningpolicy:production",
        ]
    )

    if include_check_mode:
        lines.extend(
            [
                r'if /i "%%~A"=="/check" set "ARG_CHECK=1"',
                r'if /i "%%~A"=="/validate" set "ARG_CHECK=1"',
                r'if "%ARG_CHECK%"=="1" goto :check_mode',
                r":check_mode",
                r'set "INSTALL_ROOT=%TEMP%\AeroGuestToolsCheck"',
                r"call :validate_cert_payload",
            ]
        )

    if include_skipstorage_flag:
        lines.append("/skipstorage")

    if include_storage_skip_marker:
        lines.extend(
            [
                r'set "STATE_STORAGE_SKIPPED=C:\AeroGuestTools\storage-preseed.skipped.txt"',
                r'> "%STATE_STORAGE_SKIPPED%" echo marker',
            ]
        )

    if include_cert_policy_gating:
        lines.append('if /i "%SIGNING_POLICY%"=="test" set "CERTS_REQUIRED=1"')

    if include_cert_install_skip_policy:
        lines.append("/installcerts")

    # Certificate policy gating lives in :install_certs in the real setup.cmd.
    lines.append(":install_certs")
    if include_cert_install_skip_policy:
        lines.extend(
            [
                'if /i not "%SIGNING_POLICY%"=="test" if not "%ARG_INSTALL_CERTS%"=="1" (',
                "  exit /b 0",
                ")",
            ]
        )
    lines.append('"%SYS32%\\certutil.exe" -addstore -f Root "%CERT_FILE%"')

    return "\n".join(lines) + "\n"


def _synthetic_uninstall_text() -> str:
    return "\n".join(
        [
            "testsigning.enabled-by-aero.txt",
            "nointegritychecks.enabled-by-aero.txt",
            "storage-preseed.skipped.txt",
        ]
    )


def _synthetic_verify_text() -> str:
    return "\n".join(
        [
            "# CriticalDeviceDatabase section exists",
            "CriticalDeviceDatabase",
            "storage-preseed.skipped.txt",
            "/skipstorage",
            "virtio_blk_boot_critical",
            "manifest.json",
            "signing_policy",
        ]
    )


class LintGuestToolsScriptsTests(unittest.TestCase):
    def test_linter_passes_on_repo_scripts(self) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        errs = lint_guest_tools_scripts.lint_files(
            setup_cmd=repo_root / "guest-tools/setup.cmd",
            uninstall_cmd=repo_root / "guest-tools/uninstall.cmd",
            verify_ps1=repo_root / "guest-tools/verify.ps1",
        )
        self.assertEqual(errs, [])

        # Also ensure the CLI returns success.
        stdout = io.StringIO()
        stderr = io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            rc = lint_guest_tools_scripts.main([])
        self.assertEqual(rc, 0, msg=f"stdout:\n{stdout.getvalue()}\nstderr:\n{stderr.getvalue()}")

    def test_linter_fails_when_setup_missing_cdd_base_path(self) -> None:
        # Synthetic scripts: include all other invariants, but omit the exact
        # CriticalDeviceDatabase base path used by setup.cmd.
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_cdd_base_path=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("CriticalDeviceDatabase base path" in e for e in errs),
                msg="expected missing base-path error. Errors:\n" + "\n".join(errs),
            )

            stdout = io.StringIO()
            stderr = io.StringIO()
            with redirect_stdout(stdout), redirect_stderr(stderr):
                rc = lint_guest_tools_scripts.main(
                    [
                        "--setup-cmd",
                        str(setup_cmd),
                        "--uninstall-cmd",
                        str(uninstall_cmd),
                        "--verify-ps1",
                        str(verify_ps1),
                    ]
                )
            self.assertNotEqual(rc, 0, msg=f"expected failure. stdout:\n{stdout.getvalue()}\nstderr:\n{stderr.getvalue()}")

    def test_linter_fails_when_setup_missing_skipstorage_flag(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_skipstorage_flag=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("/skipstorage" in e for e in errs),
                msg="expected missing /skipstorage error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_setup_missing_check_mode(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_check_mode=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("dry-run validation mode" in e for e in errs),
                msg="expected missing /check mode error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_setup_missing_storage_skip_marker(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_storage_skip_marker=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("storage preseed skip marker" in e for e in errs),
                msg="expected missing storage-preseed marker error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_setup_missing_cert_policy_gating(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_cert_policy_gating=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("Certificate install requirement" in e for e in errs),
                msg="expected missing cert-policy gating error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_setup_missing_cert_install_skip_policy(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_cert_install_skip_policy=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("Certificate installation is skipped by policy" in e for e in errs),
                msg="expected missing cert-install skip-policy error. Errors:\n" + "\n".join(errs),
            )


if __name__ == "__main__":
    unittest.main()
