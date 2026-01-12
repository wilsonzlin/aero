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

            setup_cmd.write_text(
                "\n".join(
                    [
                        "rem CriticalDeviceDatabase (but missing full HKLM\\\\SYSTEM... path)",
                        "CriticalDeviceDatabase",
                        "AERO_VIRTIO_BLK_SERVICE",
                        "AERO_VIRTIO_BLK_HWIDS",
                        "reg.exe add \"%SVC_KEY%\" /v Start /t REG_DWORD /d 0 /f",
                        "signing_policy=test signing_policy=production signing_policy=none",
                        "/testsigning",
                        "/nointegritychecks",
                        "/forcesigningpolicy:none /forcesigningpolicy:test /forcesigningpolicy:production",
                    ]
                ),
                encoding="utf-8",
            )
            uninstall_cmd.write_text(
                "\n".join(
                    [
                        "testsigning.enabled-by-aero.txt",
                        "nointegritychecks.enabled-by-aero.txt",
                    ]
                ),
                encoding="utf-8",
            )
            verify_ps1.write_text(
                "\n".join(
                    [
                        "# CriticalDeviceDatabase section exists",
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


if __name__ == "__main__":
    unittest.main()

