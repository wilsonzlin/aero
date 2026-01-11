#!/usr/bin/env python3

import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import List, Optional


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def _find_iso_tool() -> Optional[List[str]]:
    xorriso = shutil.which("xorriso")
    if xorriso:
        return [xorriso, "-as", "mkisofs"]
    genisoimage = shutil.which("genisoimage")
    if genisoimage:
        return [genisoimage]
    mkisofs = shutil.which("mkisofs")
    if mkisofs:
        return [mkisofs]
    return None


def _find_7z() -> Optional[str]:
    for name in ("7z", "7zz", "7za"):
        hit = shutil.which(name)
        if hit:
            return hit
    return None


def _has_pycdlib() -> bool:
    try:
        import pycdlib  # type: ignore  # noqa: F401
    except ModuleNotFoundError:
        return False
    return True


class VirtioWinExtractTest(unittest.TestCase):
    def _resolve_case_insensitive(self, root: Path, *parts: str) -> Path:
        cur = root
        for part in parts:
            if not cur.is_dir():
                raise AssertionError(f"expected directory, got: {cur}")
            hit = None
            for child in cur.iterdir():
                if child.name.casefold() == part.casefold():
                    hit = child
                    break
            if hit is None:
                raise AssertionError(f"missing path component under {cur}: {part}")
            cur = hit
        return cur

    def _has_child_case_insensitive(self, root: Path, name: str) -> bool:
        if not root.is_dir():
            return False
        return any(c.name.casefold() == name.casefold() for c in root.iterdir())

    def _assert_extract_output(self, *, out_root: Path, iso_path: Path, expect_backend: str) -> None:
        # Required content should be present.
        self.assertTrue(self._resolve_case_insensitive(out_root, "viostor", "w7.1", "amd64", "viostor.inf").is_file())
        self.assertTrue(self._resolve_case_insensitive(out_root, "viostor", "w7.1", "x86", "viostor.inf").is_file())
        self.assertTrue(self._resolve_case_insensitive(out_root, "NetKVM", "w7", "x64", "netkvm.inf").is_file())
        self.assertTrue(self._resolve_case_insensitive(out_root, "NetKVM", "w7", "i386", "netkvm.inf").is_file())

        # Optional content (only x86 present in ISO) should be extracted.
        self.assertTrue(self._resolve_case_insensitive(out_root, "vioinput", "win7", "x86", "vioinput.inf").is_file())
        vioinput_win7 = self._resolve_case_insensitive(out_root, "vioinput", "win7")
        self.assertFalse(self._has_child_case_insensitive(vioinput_win7, "amd64"))

        self.assertTrue(self._resolve_case_insensitive(out_root, "LICENSE.txt").is_file())

        # Noise should not be extracted.
        self.assertFalse(self._has_child_case_insensitive(out_root, "Balloon"))
        viostor_root = self._resolve_case_insensitive(out_root, "viostor")
        self.assertFalse(self._has_child_case_insensitive(viostor_root, "w10"))

        # Root-level notice files should be present.
        self.assertTrue(self._resolve_case_insensitive(out_root, "LICENSE.txt").is_file())
        self.assertTrue(self._resolve_case_insensitive(out_root, "README.md").is_file())
        self.assertTrue(self._resolve_case_insensitive(out_root, "VERSION").is_file())

        prov_path = out_root / "virtio-win-provenance.json"
        self.assertTrue(prov_path.is_file())
        prov = json.loads(prov_path.read_text(encoding="utf-8"))

        self.assertEqual(prov["backend"], expect_backend)
        self.assertEqual(prov["virtio_win_iso"]["sha256"], _sha256(iso_path))
        self.assertEqual(prov["virtio_win_iso"]["volume_id"], "VIRTIOWIN_TEST")
        extracted = {(e["driver"], e["arch"]) for e in prov.get("extracted", [])}
        self.assertIn(("viostor", "amd64"), extracted)
        self.assertIn(("viostor", "x86"), extracted)
        self.assertIn(("netkvm", "amd64"), extracted)
        self.assertIn(("netkvm", "x86"), extracted)
        self.assertIn(("vioinput", "x86"), extracted)

        missing = prov.get("missing_optional", [])
        self.assertTrue(any(m.get("driver") == "viosnd" for m in missing))
        self.assertTrue(any(m.get("driver") == "vioinput" and m.get("arch") == "amd64" for m in missing))

        extracted_notice = [p.casefold() for p in prov.get("extracted_notice_files", [])]
        self.assertIn("license.txt", extracted_notice)
        self.assertIn("readme.md", extracted_notice)

        extracted_metadata = [p.casefold() for p in prov.get("extracted_metadata_files", [])]
        self.assertIn("version", extracted_metadata)

    def test_extract_synthetic_iso(self) -> None:
        iso_tool = _find_iso_tool()
        if not iso_tool:
            self.skipTest("no ISO authoring tool found (need xorriso/genisoimage/mkisofs)")

        have_7z = _find_7z() is not None
        have_pycdlib = _has_pycdlib()
        if not have_7z and not have_pycdlib:
            self.skipTest("neither 7z nor pycdlib are available; install p7zip or pycdlib to run this test")

        repo_root = Path(__file__).resolve().parents[3]
        extract_script = repo_root / "tools/virtio-win/extract.py"
        self.assertTrue(extract_script.is_file(), f"missing extractor: {extract_script}")

        with tempfile.TemporaryDirectory(prefix="aero-virtio-win-extract-test-") as tmp:
            tmp_path = Path(tmp)
            stage_root = tmp_path / "iso-root"
            stage_root.mkdir(parents=True, exist_ok=True)

            def write(rel: str, content: str) -> None:
                p = stage_root / rel
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_text(content, encoding="utf-8")

            # Required drivers:
            write("viostor/w7.1/amd64/viostor.inf", "viostor amd64")
            write("viostor/w7.1/x86/viostor.inf", "viostor x86")
            write("NetKVM/w7/x64/netkvm.inf", "netkvm amd64")
            write("NetKVM/w7/i386/netkvm.inf", "netkvm x86")

            # Optional driver present for x86 only; amd64 missing should be reported.
            write("vioinput/win7/x86/vioinput.inf", "vioinput x86")

            # Root-level license/notice files should be extracted too.
            write("LICENSE.txt", "license text")
            write("README.md", "readme text")
            write("VERSION", "1.2.3-test")

            # Noise that should not be extracted.
            write("Balloon/w7/amd64/balloon.inf", "balloon")
            write("viostor/w10/amd64/should_not_extract.inf", "nope")

            iso_path = tmp_path / "virtio-win.iso"
            cmd = [
                *iso_tool,
                "-iso-level",
                "3",
                "-J",
                "-R",
                "-V",
                "VIRTIOWIN_TEST",
                "-o",
                str(iso_path),
                str(stage_root),
            ]
            subprocess.run(cmd, check=True)

            if have_7z:
                out_root = tmp_path / "out-7z"
                subprocess.run(
                    [
                        sys.executable,
                        str(extract_script),
                        "--virtio-win-iso",
                        str(iso_path),
                        "--out-root",
                        str(out_root),
                        "--backend",
                        "7z",
                    ],
                    check=True,
                )
                self._assert_extract_output(out_root=out_root, iso_path=iso_path, expect_backend="7z")

            if have_pycdlib:
                out_root = tmp_path / "out-pycdlib"
                subprocess.run(
                    [
                        sys.executable,
                        str(extract_script),
                        "--virtio-win-iso",
                        str(iso_path),
                        "--out-root",
                        str(out_root),
                        "--backend",
                        "pycdlib",
                    ],
                    check=True,
                )
                self._assert_extract_output(out_root=out_root, iso_path=iso_path, expect_backend="pycdlib")


if __name__ == "__main__":
    unittest.main()
