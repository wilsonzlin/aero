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
    include_admin_gate: bool = True,
    admin_gate_before_check_dispatch: bool = False,
    include_check_mode_temp_root: bool = True,
    include_check_mode_validate_cert_payload: bool = True,
    check_mode_extra_line: str | None = None,
    include_skipstorage_flag: bool = True,
    include_skipstorage_parse: bool = True,
    include_skipstorage_validation_gate: bool = True,
    include_skipstorage_preseed_gate: bool = True,
    include_storage_skip_marker: bool = True,
    include_cert_policy_gating: bool = True,
    include_cert_install_skip_policy: bool = True,
    include_signature_marker_files: bool = True,
    include_testsigning_policy_gate: bool = True,
    include_testsigning_parse: bool = True,
    include_nointegritychecks_parse: bool = True,
    include_forcesigningpolicy_parse: bool = True,
    include_notestsigning_parse: bool = True,
    include_installcerts_parse: bool = True,
    include_verify_media_flag: bool = True,
    include_verify_media_parse: bool = True,
    include_installed_media_state: bool = True,
) -> str:
    lines: list[str] = []
    check_mode_block: list[str] = []
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
        ]
    )

    if include_testsigning_parse:
        lines.append(r'if /i "%%~A"=="/testsigning" set "ARG_FORCE_TESTSIGN=1"')
    if include_nointegritychecks_parse:
        lines.append(r'if /i "%%~A"=="/nointegritychecks" set "ARG_FORCE_NOINTEGRITY=1"')
    if include_forcesigningpolicy_parse:
        lines.extend(
            [
                r'if /i "%%~A"=="/forcesigningpolicy:none" set "ARG_FORCE_SIGNING_POLICY=none"',
                r'if /i "%%~A"=="/forcesigningpolicy:test" set "ARG_FORCE_SIGNING_POLICY=test"',
                r'if /i "%%~A"=="/forcesigningpolicy:production" set "ARG_FORCE_SIGNING_POLICY=production"',
            ]
        )
    if include_notestsigning_parse:
        lines.append(r'if /i "%%~A"=="/notestsigning" set "ARG_SKIP_TESTSIGN=1"')
    if include_installcerts_parse:
        lines.append(r'if /i "%%~A"=="/installcerts" set "ARG_INSTALL_CERTS=1"')

    if include_verify_media_flag:
        lines.extend(
            [
                "/verify-media",
                r'if /i "%%~A"=="/verify-media" set "ARG_VERIFY_MEDIA=1"',
                r'if /i "%%~A"=="/verifymedia" set "ARG_VERIFY_MEDIA=1"',
                'if "%ARG_VERIFY_MEDIA%"=="1" (',
                "  call :verify_media_preflight",
                ")",
                ":verify_media_preflight",
                "manifest.json",
            ]
        )
    if include_verify_media_flag and not include_verify_media_parse:
        lines = [l for l in lines if "ARG_VERIFY_MEDIA=1" not in l]

    if include_admin_gate and admin_gate_before_check_dispatch:
        lines.append("call :require_admin_stdout")

    if include_check_mode:
        lines.extend(
            [
                r'if /i "%%~A"=="/check" set "ARG_CHECK=1"',
                r'if /i "%%~A"=="/validate" set "ARG_CHECK=1"',
                r'if "%ARG_CHECK%"=="1" goto :check_mode',
            ]
        )

    if include_admin_gate and not admin_gate_before_check_dispatch:
        lines.append("call :require_admin_stdout")

    if include_check_mode:
        check_mode_block.append(r":check_mode")
        if include_check_mode_temp_root:
            check_mode_block.append(r'set "INSTALL_ROOT=%TEMP%\AeroGuestToolsCheck"')
        if include_check_mode_validate_cert_payload:
            check_mode_block.append(r"call :validate_cert_payload")
        if check_mode_extra_line:
            check_mode_block.append(check_mode_extra_line)

    if include_skipstorage_flag:
        lines.append("/skipstorage")
        if include_skipstorage_parse:
            lines.append(r'if /i "%%~A"=="/skipstorage" set "ARG_SKIP_STORAGE=1"')

    if include_skipstorage_validation_gate:
        lines.extend(
            [
                'if "%ARG_SKIP_STORAGE%"=="1" (',
                "  rem skip storage INF validation",
                ") else (",
                "  call :validate_storage_service_infs || goto :fail",
                ")",
            ]
        )

    if include_skipstorage_preseed_gate:
        lines.extend(
            [
                'if "%ARG_SKIP_STORAGE%"=="1" (',
                "  call :skip_storage_preseed || goto :fail",
                ") else (",
                "  call :preseed_storage_boot || goto :fail",
                ")",
            ]
        )

    if include_storage_skip_marker:
        lines.extend(
            [
                r'set "STATE_STORAGE_SKIPPED=C:\AeroGuestTools\storage-preseed.skipped.txt"',
                r'> "%STATE_STORAGE_SKIPPED%" echo marker',
            ]
        )

    if include_signature_marker_files:
        lines.extend(
            [
                r'set "STATE_TESTSIGN=C:\AeroGuestTools\testsigning.enabled-by-aero.txt"',
                r'> "%STATE_TESTSIGN%" echo marker',
                r'set "STATE_NOINTEGRITY=C:\AeroGuestTools\nointegritychecks.enabled-by-aero.txt"',
                r'> "%STATE_NOINTEGRITY%" echo marker',
            ]
        )

    if include_cert_policy_gating:
        lines.append('if /i "%SIGNING_POLICY%"=="test" set "CERTS_REQUIRED=1"')

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

    if include_testsigning_policy_gate:
        lines.extend(
            [
                ":maybe_enable_testsigning",
                'if /i not "%SIGNING_POLICY%"=="test" if not "%ARG_FORCE_TESTSIGN%"=="1" (',
                "  exit /b 0",
                ")",
            ]
        )

    if include_installed_media_state:
        lines.extend(
            [
                r'set "STATE_INSTALLED_MEDIA=C:\AeroGuestTools\installed-media.txt"',
                "call :write_installed_media_state",
                ":write_installed_media_state",
                "installed-media.txt",
            ]
        )

    # Place :check_mode at the end so the label block does not accidentally include
    # install-mode logic (which would make the synthetic fixture fail /check invariants).
    if include_check_mode:
        lines.extend(check_mode_block)

    return "\n".join(lines) + "\n"


def _synthetic_uninstall_text() -> str:
    return "\n".join(
        [
            "testsigning.enabled-by-aero.txt",
            "nointegritychecks.enabled-by-aero.txt",
            "storage-preseed.skipped.txt",
            "installed-media.txt",
            r'if /i "%%~A"=="/cleanupstorage" set "ARG_CLEANUP_STORAGE=1"',
            r'if /i "%%~A"=="/cleanupstorageforce" set "ARG_CLEANUP_STORAGE_FORCE=1"',
            ":maybe_cleanup_storage_preseed",
            'if "%ARG_FORCE%"=="1" if not "%ARG_CLEANUP_STORAGE_FORCE%"=="1" (',
            "  exit /b 0",
            ")",
        ]
    )


def _synthetic_verify_text() -> str:
    return "\n".join(
        [
            "# CriticalDeviceDatabase section exists",
            "CriticalDeviceDatabase",
            "storage-preseed.skipped.txt",
            "/skipstorage",
            "testsigning.enabled-by-aero.txt",
            "nointegritychecks.enabled-by-aero.txt",
            "installed-media.txt",
            "virtio_blk_boot_critical",
            "manifest.json",
            "signing_policy",
        ]
    )


class LintGuestToolsScriptsTests(unittest.TestCase):
    def test_synthetic_scripts_pass_linter(self) -> None:
        # Keep synthetic fixtures in sync with all current invariants.
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertEqual(errs, [], msg="unexpected lint errors:\n" + "\n".join(errs))

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

    def test_linter_fails_when_setup_missing_skipstorage_parse(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_skipstorage_parse=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("Parses /skipstorage" in e for e in errs),
                msg="expected missing /skipstorage parse error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_setup_missing_testsigning_parse(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_testsigning_parse=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("Supports /testsigning" in e for e in errs),
                msg="expected missing /testsigning parse error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_setup_missing_nointegritychecks_parse(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_nointegritychecks_parse=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("Supports /nointegritychecks" in e for e in errs),
                msg="expected missing /nointegritychecks parse error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_setup_missing_forcesigningpolicy_parse(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_forcesigningpolicy_parse=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("Supports /forcesigningpolicy" in e for e in errs),
                msg="expected missing /forcesigningpolicy parse error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_setup_missing_notestsigning_parse(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_notestsigning_parse=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("Supports /notestsigning" in e for e in errs),
                msg="expected missing /notestsigning parse error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_setup_missing_installcerts_parse(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_installcerts_parse=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("Supports /installcerts" in e for e in errs),
                msg="expected missing /installcerts parse error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_skipstorage_does_not_gate_storage_validation(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_skipstorage_validation_gate=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("storage INF validation" in e for e in errs),
                msg="expected missing skipstorage validation gate error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_skipstorage_does_not_gate_storage_preseed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_skipstorage_preseed_gate=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("boot-critical storage pre-seeding" in e for e in errs),
                msg="expected missing skipstorage preseed gate error. Errors:\n" + "\n".join(errs),
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

    def test_linter_fails_when_check_mode_dispatch_after_admin_gate(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(
                _synthetic_setup_text(include_admin_gate=True, admin_gate_before_check_dispatch=True),
                encoding="utf-8",
            )
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("bypasses admin requirement" in e for e in errs),
                msg="expected missing /check pre-admin dispatch error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_check_mode_missing_temp_log_root(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(
                _synthetic_setup_text(include_check_mode_temp_root=False),
                encoding="utf-8",
            )
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("logs to %TEMP%" in e for e in errs),
                msg="expected missing /check temp log root error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_check_mode_missing_validate_cert_payload(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(
                _synthetic_setup_text(include_check_mode_validate_cert_payload=False),
                encoding="utf-8",
            )
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("validates certificate payload" in e for e in errs),
                msg="expected missing validate_cert_payload call error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_check_mode_contains_destructive_command(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(
                _synthetic_setup_text(check_mode_extra_line="bcdedit /set testsigning on"),
                encoding="utf-8",
            )
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("avoids system-changing actions" in e for e in errs),
                msg="expected /check destructive command error. Errors:\n" + "\n".join(errs),
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

    def test_linter_fails_when_setup_missing_verify_media_support(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_verify_media_flag=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("verify-media" in e for e in errs),
                msg="expected missing /verify-media error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_setup_missing_verify_media_parse(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_verify_media_parse=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("Parses /verify-media" in e for e in errs),
                msg="expected missing /verify-media parse error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_setup_missing_installed_media_state(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_installed_media_state=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("installed media provenance" in e for e in errs),
                msg="expected missing installed-media.txt error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_setup_missing_signature_state_markers(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_signature_marker_files=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("marker file when enabling Test Signing" in e for e in errs),
                msg="expected missing testsigning marker file error. Errors:\n" + "\n".join(errs),
            )
            self.assertTrue(
                any("marker file when enabling nointegritychecks" in e for e in errs),
                msg="expected missing nointegritychecks marker file error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_setup_missing_testsigning_policy_gate(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(include_testsigning_policy_gate=False), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("Test Signing changes are gated" in e for e in errs),
                msg="expected missing testsigning policy gate error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_verify_missing_storage_skip_marker_awareness(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(
                "\n".join(
                    [
                        "CriticalDeviceDatabase",
                        "virtio_blk_boot_critical",
                        "manifest.json",
                        "signing_policy",
                    ]
                ),
                encoding="utf-8",
            )

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("storage pre-seeding was skipped" in e for e in errs),
                msg="expected missing verify skipstorage marker awareness error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_verify_missing_signature_marker_awareness(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(
                "\n".join(
                    [
                        "CriticalDeviceDatabase",
                        "storage-preseed.skipped.txt",
                        "/skipstorage",
                        "virtio_blk_boot_critical",
                        "manifest.json",
                        "signing_policy",
                    ]
                ),
                encoding="utf-8",
            )

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("signature-mode marker files" in e for e in errs),
                msg="expected missing verify signature marker awareness error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_verify_missing_installed_media_reference(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(), encoding="utf-8")
            uninstall_cmd.write_text(_synthetic_uninstall_text(), encoding="utf-8")
            verify_ps1.write_text(
                "\n".join(
                    [
                        "CriticalDeviceDatabase",
                        "storage-preseed.skipped.txt",
                        "/skipstorage",
                        "testsigning.enabled-by-aero.txt",
                        "nointegritychecks.enabled-by-aero.txt",
                        "virtio_blk_boot_critical",
                        "manifest.json",
                        "signing_policy",
                    ]
                ),
                encoding="utf-8",
            )

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("installed media provenance" in e for e in errs),
                msg="expected missing verify installed-media.txt reference error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_uninstall_missing_storage_skip_marker_reference(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(), encoding="utf-8")
            uninstall_cmd.write_text(
                "\n".join(["testsigning.enabled-by-aero.txt", "nointegritychecks.enabled-by-aero.txt"]),
                encoding="utf-8",
            )
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("storage preseed skipped" in e for e in errs),
                msg="expected missing uninstall skipstorage marker reference error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_uninstall_missing_installed_media_reference(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(), encoding="utf-8")
            uninstall_cmd.write_text(
                "\n".join(
                    [
                        "testsigning.enabled-by-aero.txt",
                        "nointegritychecks.enabled-by-aero.txt",
                        "storage-preseed.skipped.txt",
                    ]
                ),
                encoding="utf-8",
            )
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("installed media provenance file" in e for e in errs),
                msg="expected missing uninstall installed-media.txt reference error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_fails_when_uninstall_missing_cleanupstorage_force_gate(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(), encoding="utf-8")
            # Include parsing lines but omit the force-mode gate.
            uninstall_cmd.write_text(
                "\n".join(
                    [
                        "testsigning.enabled-by-aero.txt",
                        "nointegritychecks.enabled-by-aero.txt",
                        "storage-preseed.skipped.txt",
                        "installed-media.txt",
                        r'if /i "%%~A"=="/cleanupstorage" set "ARG_CLEANUP_STORAGE=1"',
                        r'if /i "%%~A"=="/cleanupstorageforce" set "ARG_CLEANUP_STORAGE_FORCE=1"',
                        ":maybe_cleanup_storage_preseed",
                        "echo missing gate",
                    ]
                ),
                encoding="utf-8",
            )
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertTrue(errs, msg="expected lint errors, got none")
            self.assertTrue(
                any("/cleanupstorage is gated" in e for e in errs),
                msg="expected missing cleanupstorage force gate error. Errors:\n" + "\n".join(errs),
            )

    def test_linter_allows_cleanupstorage_alias_flags(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-lint-") as tmp:
            tmp_path = Path(tmp)
            setup_cmd = tmp_path / "setup.cmd"
            uninstall_cmd = tmp_path / "uninstall.cmd"
            verify_ps1 = tmp_path / "verify.ps1"

            setup_cmd.write_text(_synthetic_setup_text(), encoding="utf-8")
            uninstall_cmd.write_text(
                "\n".join(
                    [
                        "testsigning.enabled-by-aero.txt",
                        "nointegritychecks.enabled-by-aero.txt",
                        "storage-preseed.skipped.txt",
                        "installed-media.txt",
                        r'if /i "%%~A"=="/cleanup-storage" set "ARG_CLEANUP_STORAGE=1"',
                        r'if /i "%%~A"=="/cleanup-storage-force" set "ARG_CLEANUP_STORAGE_FORCE=1"',
                        ":maybe_cleanup_storage_preseed",
                        'if "%ARG_FORCE%"=="1" if "%ARG_CLEANUP_STORAGE_FORCE%"=="0" (',
                        "  exit /b 0",
                        ")",
                    ]
                ),
                encoding="utf-8",
            )
            verify_ps1.write_text(_synthetic_verify_text(), encoding="utf-8")

            errs = lint_guest_tools_scripts.lint_files(
                setup_cmd=setup_cmd, uninstall_cmd=uninstall_cmd, verify_ps1=verify_ps1
            )
            self.assertEqual(errs, [], msg="expected no lint errors. Errors:\n" + "\n".join(errs))


if __name__ == "__main__":
    unittest.main()
