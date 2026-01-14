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


def _iso9660_dir_ident(name: str) -> str:
    # ISO-9660 directory identifiers are typically restricted to A-Z0-9_.
    # For this synthetic test ISO, a conservative transform is good enough; the
    # extractor's matching is case-insensitive and tolerates common substitutions
    # like "." -> "_".
    out = []
    for c in name.upper():
        if "A" <= c <= "Z" or "0" <= c <= "9" or c == "_":
            out.append(c)
        else:
            out.append("_")
    return "".join(out) or "_"


def _iso9660_file_ident(name: str) -> str:
    # ISO-9660 files are stored with a version suffix (e.g. `;1`). We also
    # intentionally represent "no extension" as a trailing dot (`VERSION.;1`) to
    # keep coverage for extractor normalization behavior.
    base, dot, ext = name.rpartition(".")
    if dot:
        base_norm = _iso9660_dir_ident(base)
        ext_norm = _iso9660_dir_ident(ext)
        return f"{base_norm}.{ext_norm};1"
    base_norm = _iso9660_dir_ident(name)
    return f"{base_norm}.;1"


def _create_iso_pycdlib(stage_root: Path, iso_path: Path, *, joliet: bool, rock_ridge: bool) -> None:
    import pycdlib  # type: ignore

    iso = pycdlib.PyCdlib()
    new_kwargs: dict[str, object] = {
        "interchange_level": 3,
        "vol_ident": "VIRTIOWIN_TEST",
    }
    if joliet:
        # Joliet level 3 supports long mixed-case names (what virtio-win ISOs use).
        new_kwargs["joliet"] = 3
    if rock_ridge:
        # Rock Ridge v1.09 is the most common setting and is accepted by pycdlib.
        new_kwargs["rock_ridge"] = "1.09"

    # Be slightly defensive across pycdlib versions; some accept boolean values.
    try:
        iso.new(**new_kwargs)  # type: ignore[arg-type]
    except TypeError:
        compat = dict(new_kwargs)
        if "joliet" in compat:
            compat["joliet"] = True
        if "rock_ridge" in compat:
            compat["rock_ridge"] = True
        iso.new(**compat)  # type: ignore[arg-type]

    try:
        # Add directories first (parents before children).
        dirs = sorted(
            [p for p in stage_root.rglob("*") if p.is_dir()],
            key=lambda p: (len(p.relative_to(stage_root).parts), str(p).casefold()),
        )
        for d in dirs:
            rel_parts = d.relative_to(stage_root).parts
            if not rel_parts:
                continue
            iso_parts = [_iso9660_dir_ident(p) for p in rel_parts]
            iso_dir_path = "/" + "/".join(iso_parts)
            kwargs: dict[str, object] = {"iso_path": iso_dir_path}
            if joliet:
                kwargs["joliet_path"] = "/" + "/".join(rel_parts)
            if rock_ridge:
                kwargs["rr_name"] = rel_parts[-1]
            iso.add_directory(**kwargs)  # type: ignore[arg-type]

        files = sorted(
            [p for p in stage_root.rglob("*") if p.is_file()],
            key=lambda p: (len(p.relative_to(stage_root).parts), str(p).casefold()),
        )
        for f in files:
            rel_parts = f.relative_to(stage_root).parts
            dir_parts = rel_parts[:-1]
            file_name = rel_parts[-1]

            iso_dir_parts = [_iso9660_dir_ident(p) for p in dir_parts]
            iso_name = _iso9660_file_ident(file_name)
            iso_file_path = "/" + "/".join([*iso_dir_parts, iso_name]) if iso_dir_parts else "/" + iso_name

            kwargs = {"iso_path": iso_file_path}
            if joliet:
                kwargs["joliet_path"] = "/" + "/".join(rel_parts)
            if rock_ridge:
                kwargs["rr_name"] = file_name
            iso.add_file(str(f), **kwargs)  # type: ignore[arg-type]

        iso.write(str(iso_path))
    finally:
        iso.close()


def _create_iso_external(
    iso_tool: List[str],
    stage_root: Path,
    iso_path: Path,
    *,
    joliet: bool,
    rock_ridge: bool,
) -> None:
    cmd = [
        *iso_tool,
        "-iso-level",
        "3",
    ]
    if joliet:
        cmd.append("-J")
    if rock_ridge:
        cmd.append("-R")
    cmd += [
        "-V",
        "VIRTIOWIN_TEST",
        "-o",
        str(iso_path),
        str(stage_root),
    ]
    subprocess.run(cmd, check=True)


class VirtioWinExtractTest(unittest.TestCase):
    def _resolve_any_case_insensitive(self, root: Path, options: List[str]) -> Path:
        last_err: Optional[AssertionError] = None
        for opt in options:
            try:
                return self._resolve_case_insensitive(root, opt)
            except AssertionError as e:
                last_err = e
        if last_err is None:
            raise AssertionError("no options provided to _resolve_any_case_insensitive")
        raise last_err

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

    def _assert_extract_output(
        self,
        *,
        out_root: Path,
        iso_path: Path,
        expect_backend: str,
        expect_pycdlib_path_mode: Optional[str] = None,
    ) -> None:
        # Required content should be present.
        viostor_root = self._resolve_case_insensitive(out_root, "viostor")
        viostor_os = self._resolve_any_case_insensitive(viostor_root, ["w7.1", "w7_1", "w7", "win7"])
        self.assertTrue(self._resolve_case_insensitive(viostor_os, "amd64", "viostor.inf").is_file())
        self.assertTrue(self._resolve_case_insensitive(viostor_os, "x86", "viostor.inf").is_file())
        self.assertTrue(self._resolve_case_insensitive(out_root, "NetKVM", "w7", "x64", "netkvm.inf").is_file())
        self.assertTrue(self._resolve_case_insensitive(out_root, "NetKVM", "w7", "i386", "netkvm.inf").is_file())

        # Optional content (only x86 present in ISO) should be extracted.
        self.assertTrue(self._resolve_case_insensitive(out_root, "vioinput", "win7", "x86", "vioinput.inf").is_file())
        vioinput_win7 = self._resolve_case_insensitive(out_root, "vioinput", "win7")
        self.assertFalse(self._has_child_case_insensitive(vioinput_win7, "amd64"))

        self.assertTrue(self._resolve_case_insensitive(out_root, "LICENSE.txt").is_file())

        # Noise should not be extracted.
        self.assertFalse(self._has_child_case_insensitive(out_root, "Balloon"))
        self.assertFalse(self._has_child_case_insensitive(viostor_root, "w10"))

        # Root-level notice files should be present.
        self.assertTrue(self._resolve_case_insensitive(out_root, "LICENSE.txt").is_file())
        self.assertTrue(self._resolve_case_insensitive(out_root, "README.md").is_file())
        self.assertTrue(self._resolve_case_insensitive(out_root, "VERSION").is_file())

        prov_path = out_root / "virtio-win-provenance.json"
        self.assertTrue(prov_path.is_file())
        prov = json.loads(prov_path.read_text(encoding="utf-8"))

        self.assertEqual(prov["backend"], expect_backend)
        if expect_backend == "pycdlib":
            self.assertEqual(prov.get("pycdlib_path_mode"), expect_pycdlib_path_mode)
        else:
            self.assertIsNone(prov.get("pycdlib_path_mode"))
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
        have_7z = _find_7z() is not None
        have_pycdlib = _has_pycdlib()
        if not have_7z and not have_pycdlib:
            self.skipTest("neither 7z nor pycdlib are available; install p7zip or pycdlib to run this test")

        # Prefer pure-Python ISO authoring when available; fall back to external
        # tooling for local development environments without pycdlib installed.
        iso_tool: Optional[List[str]] = None
        if not have_pycdlib:
            iso_tool = _find_iso_tool()
            if not iso_tool:
                self.skipTest(
                    "no supported ISO authoring method found; install pycdlib or xorriso/genisoimage/mkisofs"
                )

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
            if have_pycdlib:
                _create_iso_pycdlib(stage_root, iso_path, joliet=True, rock_ridge=True)
            else:
                assert iso_tool is not None
                _create_iso_external(iso_tool, stage_root, iso_path, joliet=True, rock_ridge=True)

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
                self._assert_extract_output(
                    out_root=out_root,
                    iso_path=iso_path,
                    expect_backend="pycdlib",
                    expect_pycdlib_path_mode="joliet",
                )

                # Create an ISO without Joliet and ensure the extractor can fall back
                # to Rock Ridge paths when using the pycdlib backend.
                iso_rr_path = tmp_path / "virtio-win-rr.iso"
                _create_iso_pycdlib(stage_root, iso_rr_path, joliet=False, rock_ridge=True)

                out_rr_root = tmp_path / "out-pycdlib-rr"
                subprocess.run(
                    [
                        sys.executable,
                        str(extract_script),
                        "--virtio-win-iso",
                        str(iso_rr_path),
                        "--out-root",
                        str(out_rr_root),
                        "--backend",
                        "pycdlib",
                    ],
                    check=True,
                )
                self._assert_extract_output(
                    out_root=out_rr_root,
                    iso_path=iso_rr_path,
                    expect_backend="pycdlib",
                    expect_pycdlib_path_mode="rr",
                )

                # Create an ISO without Joliet/Rock Ridge and ensure the extractor can
                # fall back to ISO-9660 paths (and strips ISO version suffixes like `;1`
                # from extracted filenames).
                iso_iso_path = tmp_path / "virtio-win-iso9660.iso"
                _create_iso_pycdlib(stage_root, iso_iso_path, joliet=False, rock_ridge=False)

                out_iso_root = tmp_path / "out-pycdlib-iso"
                subprocess.run(
                    [
                        sys.executable,
                        str(extract_script),
                        "--virtio-win-iso",
                        str(iso_iso_path),
                        "--out-root",
                        str(out_iso_root),
                        "--backend",
                        "pycdlib",
                    ],
                    check=True,
                )
                self._assert_extract_output(
                    out_root=out_iso_root,
                    iso_path=iso_iso_path,
                    expect_backend="pycdlib",
                    expect_pycdlib_path_mode="iso",
                )

                if have_7z:
                    out_iso_7z_root = tmp_path / "out-7z-iso"
                    subprocess.run(
                        [
                            sys.executable,
                            str(extract_script),
                            "--virtio-win-iso",
                            str(iso_iso_path),
                            "--out-root",
                            str(out_iso_7z_root),
                            "--backend",
                            "7z",
                        ],
                        check=True,
                    )
                    self._assert_extract_output(out_root=out_iso_7z_root, iso_path=iso_iso_path, expect_backend="7z")


if __name__ == "__main__":
    unittest.main()
