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


if __name__ == "__main__":
    unittest.main()
